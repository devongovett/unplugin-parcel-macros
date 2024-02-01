import { defineConfig } from "vite";
import unplugin from 'unplugin-parcel-macros';

export default defineConfig({
  plugins: [
    unplugin.vite()
  ]
});
