import UnpluginMacros from 'unplugin-parcel-macros';

let macros = UnpluginMacros.webpack();

/** @type {import('next').NextConfig} */
const nextConfig = {
  webpack(config) {
    config.plugins.push(macros);
    return config;
  }
};

export default nextConfig;
