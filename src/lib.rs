use indexmap::IndexMap;
use napi::{Env, JsBoolean, JsFunction, JsNumber, JsObject, JsString, ValueType};
use napi_derive::napi;
use parcel_macros::JsValue;
use parcel_macros::{napi::create_macro_callback, MacroCallback, MacroError, Macros};
use swc_core::common::{BytePos, LineCol};
use swc_core::ecma::codegen::text_writer::JsWriter;
use swc_core::ecma::codegen::Emitter;
use swc_core::{common::errors::Handler, ecma::visit::FoldWith};
use swc_core::{
  common::{
    comments::SingleThreadedComments, source_map::SourceMapGenConfig, sync::Lrc, FileName, Globals,
    Mark, SourceMap,
  },
  ecma::{
    ast::{Module, ModuleItem, Program},
    parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax},
    transforms::base::resolver,
  },
};
use swc_error_reporters::handler::ThreadSafetyDiagnostics;
use swc_error_reporters::{ErrorEmitter, GraphicalReportHandler, ToPrettyDiagnostic};

mod resolver;

#[napi]
pub enum Type {
  JS,
  JSX,
  TS,
  TSX,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn transform(
  env: Env,
  ty: Type,
  filename: String,
  code: String,
  call_macro: JsFunction,
) -> napi::Result<JsObject> {
  let call_macro = create_macro_callback(call_macro, env)?;
  let (deferred, promise) = env.create_deferred()?;

  rayon::spawn(move || {
    let res = transform_internal(ty, filename, code, call_macro);
    match res {
      Ok(result) => deferred.resolve(move |_| Ok(result)),
      Err(err) => deferred.reject(err.into()),
    }
  });

  Ok(promise)
}

#[cfg(target_arch = "wasm32")]
#[napi]
pub fn transform(
  env: Env,
  ty: Type,
  filename: String,
  code: String,
  call_macro: JsFunction,
) -> napi::Result<TransformResult> {
  use napi::{Ref, JsUnknown, NapiRaw, NapiValue, bindgen_prelude::FromNapiValue};
  use parcel_macros::Location;
  use swc_core::common::DUMMY_SP;
  use std::sync::Arc;

  // This relies on Binaryen's Asyncify transform to allow Rust to call async JS functions from sync code.
  // See the comments in wasm.mjs for more details about how this works.
  extern "C" {
    fn await_promise_sync(
      promise: napi::sys::napi_value,
      result: *mut napi::sys::napi_value,
      error: *mut napi::sys::napi_value,
    );
  }

  #[napi(object)]
  struct JsMacroError {
    pub kind: u32,
    pub message: String,
  }

  fn call(
    env: Env,
    fn_ref: &Ref<()>,
    src: String,
    export: String,
    args: Vec<JsValue>,
    loc: Location,
  ) -> napi::Result<Result<JsValue, MacroError>> {
    let call_macro: JsFunction = env.get_reference_value_unchecked(&fn_ref)?;
    let null = env.get_null()?.into_unknown();
    let src = env.create_string_from_std(src)?.into_unknown();
    let export = env.create_string_from_std(export)?.into_unknown();
    let args = js_value_to_napi(JsValue::Array(args), env)?;
    let loc = env.to_js_value(&loc)?;
    let value: JsUnknown = call_macro.call(None, &[null, src, export, args, loc])?;

    if value.is_promise()? {
      let mut result = std::ptr::null_mut();
      let mut error = std::ptr::null_mut();
      unsafe { await_promise_sync(value.raw(), &mut result, &mut error) };
      if !error.is_null() {
        let error = unsafe { JsUnknown::from_raw(env.raw(), error)? };
        let error = JsMacroError::from_unknown(error)?;
        let err = match error.kind {
          1 => MacroError::LoadError(error.message, DUMMY_SP),
          2 => MacroError::ExecutionError(error.message, DUMMY_SP),
          _ => MacroError::LoadError("Invalid error kind".into(), DUMMY_SP),
        };
        return Ok(Err(err));
      }

      let value = unsafe { JsUnknown::from_raw(env.raw(), result)? };
      Ok(Ok(napi_to_js_value(value, env)?))
    } else {
      Ok(Ok(napi_to_js_value(value, env)?))
    }
  }

  struct RefWrapper(Ref<()>, usize);
  impl Drop for RefWrapper {
    fn drop(&mut self) {
      let env = unsafe { Env::from_raw(self.1 as _) };
      drop(self.0.unref(env))
    }
  }

  let unsafe_env = env.raw() as usize;
  let fn_ref = RefWrapper(env.create_reference(call_macro)?, unsafe_env);
  let call_macro = Arc::new(move |src, export, args, loc| {
    let env = unsafe { Env::from_raw(unsafe_env as _) };
    match call(env, &fn_ref.0, src, export, args, loc) {
      Ok(v) => v,
      Err(e) => Err(MacroError::ExecutionError(e.to_string(), DUMMY_SP)),
    }
  });
  let res = transform_internal(ty, filename, code, call_macro);
  res
}

#[napi(object)]
pub struct TransformResult {
  pub code: String,
  pub map: String,
}

fn transform_internal(
  ty: Type,
  filename: String,
  code: String,
  call_macro: MacroCallback,
) -> Result<TransformResult, napi::Error> {
  let source_map = Lrc::new(SourceMap::default());
  let source_file = source_map.new_source_file(Lrc::new(FileName::Real(filename.into())), code);
  let comments = SingleThreadedComments::default();
  let mut parser = Parser::new(
    match ty {
      Type::JS | Type::JSX => {
        Syntax::Es(EsSyntax {
          // always enable JSX in .js files?
          jsx: true,
          import_attributes: true,
          ..Default::default()
        })
      }
      Type::TS | Type::TSX => Syntax::Typescript(TsSyntax {
        tsx: matches!(ty, Type::TSX),
        ..Default::default()
      }),
    },
    StringInput::from(&*source_file),
    Some(&comments),
  );

  let mut diagnostics = ThreadSafetyDiagnostics::default();
  let emitter = ErrorEmitter {
    diagnostics: diagnostics.clone(),
    cm: source_map.clone(),
    opts: Default::default(),
  };

  let handler: Handler = Handler::with_emitter(true, false, Box::new(emitter));

  let mut module = match parser.parse_program() {
    Ok(m) => m,
    Err(e) => {
      e.into_diagnostic(&handler).emit();
      let report_handler = GraphicalReportHandler::default();
      let diagnostics = diagnostics.take();
      let diagnostics_pretty_message = diagnostics
        .iter()
        .map(|d| d.to_pretty_diagnostic(&source_map, false))
        .map(|d| d.to_pretty_string(&report_handler))
        .collect::<Vec<String>>()
        .join("");
      return Err(napi::Error::new(
        napi::Status::GenericFailure,
        diagnostics_pretty_message,
      ));
    }
  };

  let mut errors = Vec::new();
  let module = swc_core::common::GLOBALS.set(&Globals::new(), || {
    let global_mark = Mark::fresh(Mark::root());
    let unresolved_mark = Mark::fresh(Mark::root());

    module.mutate(resolver(unresolved_mark, global_mark, false));
    module.fold_with(&mut Macros::new(call_macro, &source_map, &mut errors))
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

    let report_handler = GraphicalReportHandler::default();
    let diagnostics = diagnostics.take();
    let diagnostics_pretty_message = diagnostics
      .iter()
      .map(|d| d.to_pretty_diagnostic(&source_map, false))
      .map(|d| d.to_pretty_string(&report_handler))
      .collect::<Vec<String>>()
      .join("");

    return Err(napi::Error::new(
      napi::Status::GenericFailure,
      diagnostics_pretty_message,
    ));
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

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn napi_wasm_malloc(size: usize) -> *mut u8 {
  use std::alloc::{alloc, Layout};
  use std::mem;

  let align = mem::align_of::<usize>();
  if let Ok(layout) = Layout::from_size_align(size, align) {
    unsafe {
      if layout.size() > 0 {
        let ptr = alloc(layout);
        if !ptr.is_null() {
          return ptr;
        }
      } else {
        return align as *mut u8;
      }
    }
  }

  std::process::abort();
}

/// Convert a JsValue macro argument from the transformer to a napi value.
fn js_value_to_napi(value: JsValue, env: Env) -> napi::Result<napi::JsUnknown> {
  match value {
    JsValue::Undefined => Ok(env.get_undefined()?.into_unknown()),
    JsValue::Null => Ok(env.get_null()?.into_unknown()),
    JsValue::Bool(b) => Ok(env.get_boolean(b)?.into_unknown()),
    JsValue::Number(n) => Ok(env.create_double(n)?.into_unknown()),
    JsValue::String(s) => Ok(env.create_string_from_std(s)?.into_unknown()),
    JsValue::Regex { source, flags } => {
      let regexp_class: JsFunction = env.get_global()?.get_named_property("RegExp")?;
      let source = env.create_string_from_std(source)?;
      let flags = env.create_string_from_std(flags)?;
      let re = regexp_class.new_instance(&[source, flags])?;
      Ok(re.into_unknown())
    }
    JsValue::Array(arr) => {
      let mut res = env.create_array(arr.len() as u32)?;
      for (i, val) in arr.into_iter().enumerate() {
        res.set(i as u32, js_value_to_napi(val, env)?)?;
      }
      Ok(res.coerce_to_object()?.into_unknown())
    }
    JsValue::Object(obj) => {
      let mut res = env.create_object()?;
      for (k, v) in obj {
        res.set_named_property(&k, js_value_to_napi(v, env)?)?;
      }
      Ok(res.into_unknown())
    }
    JsValue::Function(_) => {
      // Functions can only be returned from macros, not passed in.
      unreachable!()
    }
  }
}

/// Convert a napi value returned as a result of a macro to a JsValue for the transformer.
fn napi_to_js_value(value: napi::JsUnknown, env: Env) -> napi::Result<JsValue> {
  match value.get_type()? {
    ValueType::Undefined => Ok(JsValue::Undefined),
    ValueType::Null => Ok(JsValue::Null),
    ValueType::Number => Ok(JsValue::Number(
      unsafe { value.cast::<JsNumber>() }.get_double()?,
    )),
    ValueType::Boolean => Ok(JsValue::Bool(
      unsafe { value.cast::<JsBoolean>() }.get_value()?,
    )),
    ValueType::String => Ok(JsValue::String(
      unsafe { value.cast::<JsString>() }
        .into_utf8()?
        .into_owned()?,
    )),
    ValueType::Object => {
      let obj = unsafe { value.cast::<JsObject>() };
      if obj.is_array()? {
        let len = obj.get_array_length()?;
        let mut arr = Vec::with_capacity(len as usize);
        for i in 0..len {
          let elem = napi_to_js_value(obj.get_element(i)?, env)?;
          arr.push(elem);
        }
        Ok(JsValue::Array(arr))
      } else {
        let regexp_class: JsFunction = env.get_global()?.get_named_property("RegExp")?;
        if obj.instanceof(regexp_class)? {
          let source: JsString = obj.get_named_property("source")?;
          let flags: JsString = obj.get_named_property("flags")?;
          return Ok(JsValue::Regex {
            source: source.into_utf8()?.into_owned()?,
            flags: flags.into_utf8()?.into_owned()?,
          });
        }

        let names = obj.get_property_names()?;
        let len = names.get_array_length()?;
        let mut props = IndexMap::with_capacity(len as usize);
        for i in 0..len {
          let prop = names.get_element::<JsString>(i)?;
          let name = prop.into_utf8()?.into_owned()?;
          let value = napi_to_js_value(obj.get_property(prop)?, env)?;
          props.insert(name, value);
        }
        Ok(JsValue::Object(props))
      }
    }
    ValueType::Function => {
      let f = unsafe { value.cast::<JsFunction>() };
      let source = f.coerce_to_string()?.into_utf8()?.into_owned()?;
      Ok(JsValue::Function(source))
    }
    ValueType::Symbol | ValueType::External | ValueType::Unknown => Err(napi::Error::new(
      napi::Status::GenericFailure,
      "Could not convert value returned from macro to AST.",
    )),
  }
}
