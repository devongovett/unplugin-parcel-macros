# unplugin-parcel-macros

An [Unplugin](https://unplugin.vercel.app) that lets you use Parcel's [macro](https://parceljs.org/features/macros/) implementation in webpack, Vite, Rollup, esbuild, Next.js, and more.

Macros are JavaScript functions that run at build time. The value returned by a macro is inlined into the bundle in place of the original function call. This allows you to generate constants, code, and even additional assets without any custom plugins.

Macros are imported using an [import attribute](https://github.com/tc39/proposal-import-attributes) to indicate that they should run at build time rather than being bundled into the output. You can import any JavaScript or TypeScript module as a macro, including built-in Node modules and packages from npm.

## Example

This example uses the [regexgen](https://github.com/devongovett/regexgen) library to generate an optimized regular expression from a set of strings at build time.

```js
import regexgen from 'regexgen' with {type: 'macro'};

const regex = regexgen(['foobar', 'foobaz', 'foozap', 'fooza']);
console.log(regex);
```

This compiles to the following bundle:

```js
console.log(/foo(?:zap?|ba[rz])/);
```

As you can see, the `regexgen` library has been completely compiled away, and we are left with a static regular expression!

## Setup

### webpack

```js
// webpack.config.js
const macros = require('unplugin-parcel-macros');

module.exports = {
  // ...
  plugins: [
    macros.webpack()
  ]
};
```

### Next.js

```js
// next.config.js
const macros = require('unplugin-parcel-macros');

// Create a single instance of the plugin that's shared between server and client builds.
let plugin = macros.webpack();

module.exports = {
  webpack(config) {
    config.plugins.push(plugin);
    return config;
  }
};
```

### Vite

```js
// vite.config.js
import macros from 'unplugin-parcel-macros';

export default {
  plugins: [
    macros.vite()
  ]
};
```

### Rollup

```js
// rollup.config.js
import macros from 'unplugin-parcel-macros';

export default {
  plugins: [
    macros.rollup()
  ]
};
```

### Esbuild

```js
import {build} from 'esbuild';
import macros from 'unplugin-parcel-macros';

build({
  plugins: [
    macros.esbuild()
  ]
});
```
