import {UnpluginInstance} from 'unplugin';

export interface MacroContext {
  addAsset: (asset: {type: string; content: string}) => void;
  invalidateOnFileChange: (filePath: string) => void;
}

declare const plugin: UnpluginInstance<void>;
export = plugin;
