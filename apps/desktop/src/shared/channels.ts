/**
 * The only IPC channels between the renderer and main. They are constants, not
 * something the renderer can compose: a channel name that is not listed here
 * does not exist (design 4.6).
 */
export const Channels = {
  createTask: 'desktop:create-task',
  confirm: 'desktop:confirm',
  pause: 'desktop:pause',
  resume: 'desktop:resume',
  stop: 'desktop:stop',
  setMode: 'desktop:set-mode',
  snapshot: 'desktop:snapshot',
  /** main to renderer */
  projection: 'desktop:projection',
  /** main to renderer */
  stopStatus: 'desktop:stop-status',
} as const;

export type InvokeChannel = Exclude<(typeof Channels)[keyof typeof Channels], typeof Channels.projection | typeof Channels.stopStatus>;

export const INVOKE_CHANNELS: readonly InvokeChannel[] = [
  Channels.createTask,
  Channels.confirm,
  Channels.pause,
  Channels.resume,
  Channels.stop,
  Channels.setMode,
  Channels.snapshot,
];
