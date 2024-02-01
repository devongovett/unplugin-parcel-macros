use napi::{Env, JsFunction, JsObject};
use napi_derive::napi;
use parcel_macros::{napi::create_macro_callback, MacroCallback, MacroError, Macros};
use std::sync::{Arc, Mutex};
use swc_core::common::{BytePos, LineCol};
use swc_core::ecma::codegen::text_writer::JsWriter;
use swc_core::ecma::codegen::Emitter;
use swc_core::ecma::parser::EsConfig;
use swc_core::{common::errors::Handler, ecma::visit::FoldWith};
use swc_core::{
  common::{
    chain, comments::SingleThreadedComments, source_map::SourceMapGenConfig, sync::Lrc, FileName,
    Globals, Mark, SourceMap,
  },
  ecma::{
    ast::{Module, ModuleItem, Program},
    parser::{Parser, StringInput, Syntax, TsConfig},
    transforms::base::resolver,
  },
};
use swc_error_reporters::{GraphicalReportHandler, PrettyEmitter};

#[cfg(target_os = "macos")]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[cfg(windows)]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[napi]
pub enum Type {
  JS,
  JSX,
  TS,
  TSX,
}

#[napi]
pub fn transform(
  env: Env,
  ty: Type,
  code: String,
  call_macro: JsFunction,
) -> napi::Result<JsObject> {
  let call_macro = create_macro_callback(call_macro, env)?;
  let (deferred, promise) = env.create_deferred()?;

  rayon::spawn(move || {
    let res = transform_internal(ty, code, call_macro);
    match res {
      Ok(result) => deferred.resolve(move |_| Ok(result)),
      Err(err) => deferred.reject(err.into()),
    }
  });

  Ok(promise)
}

#[napi(object)]
struct TransformResult {
  pub code: String,
  pub map: String,
}

fn transform_internal(
  ty: Type,
  code: String,
  call_macro: MacroCallback,
) -> Result<TransformResult, napi::Error> {
  let source_map = Lrc::new(SourceMap::default());
  let source_file = source_map.new_source_file(FileName::Real("test.js".into()), code);
  let comments = SingleThreadedComments::default();
  let mut parser = Parser::new(
    match ty {
      Type::JS | Type::JSX => {
        Syntax::Es(EsConfig {
          // always enable JSX in .js files?
          jsx: true,
          import_attributes: true,
          ..Default::default()
        })
      }
      Type::TS | Type::TSX => Syntax::Typescript(TsConfig {
        tsx: matches!(ty, Type::TSX),
        ..Default::default()
      }),
    },
    StringInput::from(&*source_file),
    Some(&comments),
  );

  let wr = Box::new(LockedWriter::default());
  let emitter = PrettyEmitter::new(
    source_map.clone(),
    wr.clone(),
    GraphicalReportHandler::new().with_context_lines(3),
    Default::default(),
  );
  let handler: Handler = Handler::with_emitter(true, false, Box::new(emitter));

  let module = match parser.parse_program() {
    Ok(m) => m,
    Err(e) => {
      e.into_diagnostic(&handler).emit();
      let s = &*wr.0.lock().unwrap().clone();
      return Err(napi::Error::new(napi::Status::GenericFailure, s));
    }
  };

  let mut errors = Vec::new();
  let module = swc_core::common::GLOBALS.set(&Globals::new(), || {
    let global_mark = Mark::fresh(Mark::root());
    let unresolved_mark = Mark::fresh(Mark::root());

    module.fold_with(&mut chain!(
      resolver(unresolved_mark, global_mark, false),
      &mut Macros::new(call_macro, &source_map, &mut errors)
    ))
  });

  if !errors.is_empty() {
    for error in errors {
      match error {
        MacroError::EvaluationError(span) => {
          handler
            .struct_span_err(span, "Could not statically evaluate macro argument")
            .emit();
        }
        MacroError::LoadError(err, span) => {
          handler
            .struct_span_err(span, &format!("Error loading macro: {}", err))
            .emit();
        }
        MacroError::ExecutionError(err, span) => {
          handler
            .struct_span_err(span, &format!("Error evaluating macro: {}", err))
            .emit();
        }
        MacroError::ParseError(error) => {
          error.into_diagnostic(&handler).emit();
        }
      }
    }
    let s = &*wr.0.lock().unwrap().clone();
    return Err(napi::Error::new(napi::Status::GenericFailure, s));
  }

  let module = match module {
    Program::Module(module) => module,
    Program::Script(script) => Module {
      span: script.span,
      shebang: None,
      body: script.body.into_iter().map(ModuleItem::Stmt).collect(),
    },
  };

  let (buf, src_map_buf) = emit(source_map.clone(), comments, &module)?;
  let mut map_buf = Vec::new();
  source_map
    .build_source_map_with_config(&src_map_buf, None, SourceMapConfig)
    .to_writer(&mut map_buf)
    .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;

  Ok(TransformResult {
    code: String::from_utf8(buf).unwrap(),
    map: String::from_utf8(map_buf).unwrap(),
  })
}

#[derive(Clone, Default)]
struct LockedWriter(Arc<Mutex<String>>);
impl std::fmt::Write for LockedWriter {
  fn write_str(&mut self, s: &str) -> std::fmt::Result {
    self.0.lock().unwrap().push_str(s);
    Ok(())
  }
}

// Exclude macro expansions from source maps.
struct SourceMapConfig;
impl SourceMapGenConfig for SourceMapConfig {
  fn file_name_to_source(&self, f: &FileName) -> String {
    f.to_string()
  }

  fn skip(&self, f: &FileName) -> bool {
    matches!(f, FileName::MacroExpansion | FileName::Internal(..))
  }
}

type SourceMapBuffer = Vec<(BytePos, LineCol)>;
fn emit(
  source_map: Lrc<SourceMap>,
  comments: SingleThreadedComments,
  module: &Module,
) -> Result<(Vec<u8>, SourceMapBuffer), std::io::Error> {
  let mut src_map_buf = vec![];
  let mut buf = vec![];
  {
    let writer = Box::new(JsWriter::new(
      source_map.clone(),
      "\n",
      &mut buf,
      Some(&mut src_map_buf),
    ));
    let mut emitter = Emitter {
      cfg: Default::default(),
      comments: Some(&comments),
      cm: source_map,
      wr: writer,
    };

    emitter.emit_module(module)?;
  }

  Ok((buf, src_map_buf))
}
