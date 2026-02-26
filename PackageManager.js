// This is a stripped down version of the NodePackageManager from Parcel.
const fs = require('fs');
const Module = require('module');
const path = require('path');
const { Resolver, init } = require('#native');
const { pathToFileURL } = require('url');
const { transformSync } = require('@swc/core');

// Package.json fields. Must match package_json.rs.
const MAIN = 1 << 0;
const SOURCE = 1 << 2;
const NODE_CONDITION = 1 << 3;
const SOURCE_CONDITION = 1 << 17;
const ENTRIES = MAIN | SOURCE;
const CONDITIONS = NODE_CONDITION | SOURCE_CONDITION;
const NODE_MODULES = `${path.sep}node_modules${path.sep}`;
const IS_FILE = 1 << 0;
const IS_DIR = 1 << 1;
const IS_SYMLINK = 1 << 2;

// There can be more than one instance of NodePackageManager, but node has only a single module cache.
// Therefore, the resolution cache and the map of parent to child modules should also be global.
const cache = new Map();
const children = new Map();
const invalidationsCache = new Map();

// This implements a package manager for Node by monkey patching the Node require
// algorithm so that it uses the specified FileSystem instead of the native one.
// It also handles installing packages when they are required if not already installed.
// See https://github.com/nodejs/node/blob/master/lib/internal/modules/cjs/loader.js
// for reference to Node internals.
class NodePackageManager {
  constructor(projectRoot, installer) {
    this.projectRoot = projectRoot;
    this.installer = installer;

    // $FlowFixMe - no type for _extensions
    this.currentExtensions = Object.keys(Module._extensions).map((e) =>
      e.substring(1),
    );
  }

  _createResolver() {
    return new Resolver(this.projectRoot, {
      fs:
        !init && process.versions.pnp == null
          ? undefined
          : {
              read: (path) => fs.readFileSync(path),
              kind: (path) => {
                let flags = 0;
                try {
                  let stat = fs.lstatSync(path);
                  if (stat.isSymbolicLink()) {
                    flags |= IS_SYMLINK;
                    stat = fs.statSync(path);
                  }
                  if (stat.isFile()) {
                    flags |= IS_FILE;
                  } else if (stat.isDirectory()) {
                    flags |= IS_DIR;
                  }
                } catch (err) {
                  // ignore
                }
                return flags;
              },
              readLink: (path) => fs.readlinkSync(path),
            },
      mode: 2,
      entries: ENTRIES,
      conditions: CONDITIONS,
      packageExports: true,
      moduleDirResolver:
        process.versions.pnp != null
          ? (module, from) => {
              // $FlowFixMe[prop-missing]
              let pnp = Module.findPnpApi(path.dirname(from));
              return pnp.resolveToUnqualified(
                // append slash to force loading builtins from npm
                module + "/",
                from,
              );
            }
          : undefined,
      extensions: this.currentExtensions,
      typescript: true,
    });
  }

  async require(name, from, opts) {
    let { resolved, type } = await this.resolve(name, from, opts);
    if (type === 2) {
      // On Windows, Node requires absolute paths to be file URLs.
      if (process.platform === "win32" && path.isAbsolute(resolved)) {
        resolved = pathToFileURL(resolved);
      }

      // $FlowFixMe
      return import(resolved);
    }
    return this.load(resolved, from);
  }

  requireSync(name, from) {
    let { resolved } = this.resolveSync(name, from);
    return this.load(resolved, from);
  }

  load(filePath, from) {
    if (!path.isAbsolute(filePath)) {
      // Node builtin module
      // $FlowFixMe
      return require(filePath);
    }

    // $FlowFixMe[prop-missing]
    const cachedModule = Module._cache[filePath];
    if (cachedModule !== undefined) {
      return cachedModule.exports;
    }

    // $FlowFixMe
    let m = new Module(filePath, Module._cache[from] || module.parent);

    // $FlowFixMe _extensions not in type
    const extensions = Object.keys(Module._extensions);
    // This handles supported extensions changing due to, for example, esbuild/register being used
    // We assume that the extension list will change in size - as these tools usually add support for
    // additional extensions.
    if (extensions.length !== this.currentExtensions.length) {
      this.currentExtensions = extensions.map((e) => e.substring(1));
      this.resolver = this._createResolver();
    }

    // $FlowFixMe[prop-missing]
    Module._cache[filePath] = m;

    // Patch require within this module so it goes through our require
    m.require = (id) => {
      return this.requireSync(id, filePath);
    };

    if (!filePath.includes(NODE_MODULES)) {
      let extname = path.extname(filePath);
      if (
        (extname === ".ts" ||
          extname === ".tsx" ||
          extname === ".mts" ||
          extname === ".cts") &&
        // $FlowFixMe
        !Module._extensions[extname]
      ) {
        let compile = m._compile;
        m._compile = (code, filename) => {
          let out = transformSync(code, {
            filename,
            module: {
              type: "commonjs",
              ignoreDynamic: true,
            },
          });
          compile.call(m, out.code, filename);
        };

        // $FlowFixMe
        Module._extensions[extname] = (m, filename) => {
          // $FlowFixMe
          delete Module._extensions[extname];
          // $FlowFixMe
          Module._extensions[".js"](m, filename);
        };
      }
    }

    try {
      m.load(filePath);
    } catch (err) {
      // $FlowFixMe[prop-missing]
      delete Module._cache[filePath];
      throw err;
    }
    return m.exports;
  }

  async resolve(id, from, options) {
    let basedir = path.dirname(from);
    let key = basedir + ":" + id;
    let resolved = cache.get(key);
    if (!resolved) {
      let [name] = getModuleParts(id);
      resolved = this.resolveInternal(id, from);
      cache.set(key, resolved);
      invalidationsCache.clear();

      // Add the specifier as a child to the parent module.
      // Don't do this if the specifier was an absolute path, as this was likely a dynamically resolved path
      // (e.g. babel uses require() to load .babelrc.js configs and we don't want them to be added  as children of babel itself).
      if (!path.isAbsolute(name)) {
        let moduleChildren = children.get(from);
        if (!moduleChildren) {
          moduleChildren = new Set();
          children.set(from, moduleChildren);
        }
        moduleChildren.add(name);
      }
    }

    return resolved;
  }

  resolveSync(name, from) {
    let basedir = path.dirname(from);
    let key = basedir + ":" + name;
    let resolved = cache.get(key);
    if (!resolved) {
      resolved = this.resolveInternal(name, from);
      cache.set(key, resolved);
      invalidationsCache.clear();
      if (!path.isAbsolute(name)) {
        let moduleChildren = children.get(from);
        if (!moduleChildren) {
          moduleChildren = new Set();
          children.set(from, moduleChildren);
        }
        moduleChildren.add(name);
      }
    }
    return resolved;
  }

  getInvalidations(name, from) {
    let basedir = path.dirname(from);
    let cacheKey = basedir + ":" + name;
    let resolved = cache.get(cacheKey);
    if (resolved && path.isAbsolute(resolved.resolved)) {
      let cached = invalidationsCache.get(resolved.resolved);
      if (cached != null) {
        return cached;
      }
      let res = {
        invalidateOnFileCreate: [],
        invalidateOnFileChange: new Set(),
        invalidateOnStartup: false,
      };
      let seen = new Set();
      let addKey = (name, from) => {
        let basedir = path.dirname(from);
        let key = basedir + ":" + name;
        if (seen.has(key)) {
          return;
        }
        seen.add(key);
        let resolved = cache.get(key);
        if (!resolved || !path.isAbsolute(resolved.resolved)) {
          return;
        }
        res.invalidateOnFileCreate.push(...resolved.invalidateOnFileCreate);
        res.invalidateOnFileChange.add(resolved.resolved);
        for (let file of resolved.invalidateOnFileChange) {
          res.invalidateOnFileChange.add(file);
        }
        let moduleChildren = children.get(resolved.resolved);
        if (moduleChildren) {
          for (let specifier of moduleChildren) {
            addKey(specifier, resolved.resolved);
          }
        }
      };
      addKey(name, from);

      // If this is an ES module, we won't have any of the dependencies because import statements
      // cannot be intercepted. Instead, ask the resolver to parse the file and recursively analyze the deps.
      if (resolved.type === 2) {
        let invalidations = this.resolver.getInvalidations(resolved.resolved);
        invalidations.invalidateOnFileChange.forEach((i) =>
          res.invalidateOnFileChange.add(i),
        );
        invalidations.invalidateOnFileCreate.forEach((i) =>
          res.invalidateOnFileCreate.push(i),
        );
        res.invalidateOnStartup ||= invalidations.invalidateOnStartup;
      }
      invalidationsCache.set(resolved.resolved, res);
      return res;
    }

    return {
      invalidateOnFileCreate: [],
      invalidateOnFileChange: new Set(),
      invalidateOnStartup: false,
    };
  }

  invalidate(name, from) {
    let seen = new Set();
    let invalidate = (name, from) => {
      let basedir = path.dirname(from);
      let key = basedir + ":" + name;
      if (seen.has(key)) {
        return;
      }
      seen.add(key);
      let resolved = cache.get(key);
      if (!resolved || !path.isAbsolute(resolved.resolved)) {
        return;
      }
      invalidationsCache.delete(resolved.resolved);

      // $FlowFixMe
      let module = Module._cache[resolved.resolved];
      if (module) {
        // $FlowFixMe
        delete Module._cache[resolved.resolved];
      }
      let moduleChildren = children.get(resolved.resolved);
      if (moduleChildren) {
        for (let specifier of moduleChildren) {
          invalidate(specifier, resolved.resolved);
        }
      }
      children.delete(resolved.resolved);
      cache.delete(key);
    };
    invalidate(name, from);
    this.resolver = this._createResolver();
  }

  resolveInternal(name, from) {
    if (this.resolver == null) {
      this.resolver = this._createResolver();
    }

    let res = this.resolver.resolve({
      filename: name,
      specifierType: "commonjs",
      parent: from,
    });

    // Invalidate whenever the .pnp.js file changes.
    // TODO: only when we actually resolve a node_modules package?
    if (process.versions.pnp != null && res.invalidateOnFileChange) {
      // $FlowFixMe[prop-missing]
      let pnp = Module.findPnpApi(path.dirname(from));
      res.invalidateOnFileChange.push(pnp.resolveToUnqualified("pnpapi", null));
    }

    if (res.error) {
      let e = new Error(`Could not resolve module "${name}" from "${from}"`);
      // $FlowFixMe
      e.code = "MODULE_NOT_FOUND";
      throw e;
    }

    switch (res.resolution.type) {
      case "Path": {
        let self = this;
        let resolved = res.resolution.value;
        return {
          resolved,
          invalidateOnFileChange: new Set(res.invalidateOnFileChange),
          invalidateOnFileCreate: res.invalidateOnFileCreate,
          type: res.moduleType,
          get pkg() {
            let pkgPath = self.fs.findAncestorFile(
              ["package.json"],
              resolved,
              self.projectRoot,
            );
            return pkgPath
              ? JSON.parse(self.fs.readFileSync(pkgPath, "utf8"))
              : null;
          },
        };
      }
      case "Builtin": {
        let { scheme, module } = res.resolution.value;
        return {
          resolved: scheme ? `${scheme}:${module}` : module,
          invalidateOnFileChange: new Set(res.invalidateOnFileChange),
          invalidateOnFileCreate: res.invalidateOnFileCreate,
          type: res.moduleType,
        };
      }
      default:
        throw new Error("Unknown resolution type");
    }
  }
}

function getModuleParts(_name) {
  let name = path.normalize(_name);
  let splitOn = name.indexOf(path.sep);
  if (name.charAt(0) === '@') {
    splitOn = name.indexOf(path.sep, splitOn + 1);
  }
  if (splitOn < 0) {
    return [normalizeSeparators(name), undefined];
  } else {
    return [
      normalizeSeparators(name.substring(0, splitOn)),
      name.substring(splitOn + 1) || undefined,
    ];
  }
}

const SEPARATOR_REGEX = /[\\]+/g;
function normalizeSeparators(filePath) {
  return filePath.replace(SEPARATOR_REGEX, '/');
}

module.exports = NodePackageManager;
