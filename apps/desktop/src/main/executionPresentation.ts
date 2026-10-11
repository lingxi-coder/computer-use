export interface Box { x: number; y: number; width: number; height: number }
export type ExecutionPresentation = 'console' | 'indicator';
export type NativeExecutionDomain = 'foreground_native' | 'background_ax' | null;
export type ExecutionPresentationTransition = 'enter_indicator' | 'update_indicator' | 'restore_console' | 'keep_console';

/** Only foreground-native execution may move the console into the indicator. */
export function executionPresentationTransition(executing: boolean, domain: NativeExecutionDomain,
  current: ExecutionPresentation): ExecutionPresentationTransition {
  if (executing && domain === 'foreground_native') {
    return current === 'indicator' ? 'update_indicator' : 'enter_indicator';
  }
  return current === 'indicator' ? 'restore_console' : 'keep_console';
}

function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

/** Only current target geometry can place an indicator. If no free area fits,
 * hide the console; tray and global Stop remain available. */
export function indicatorBounds(target: readonly number[] | null | undefined, areas: readonly Box[]): Box | null {
  if (!target || target.length !== 4 || !target.every(Number.isSafeInteger) || target[2]! <= 0 || target[3]! <= 0) return null;
  const rect = { x: target[0]!, y: target[1]!, width: target[2]!, height: target[3]! };
  for (const area of areas) {
    const width = 280, height = 124, inset = 12;
    if (area.width < width + inset * 2 || area.height < height + inset * 2) continue;
    const positions = [
      [area.x + area.width - width - inset, area.y + inset], [area.x + inset, area.y + inset],
      [area.x + area.width - width - inset, area.y + area.height - height - inset], [area.x + inset, area.y + area.height - height - inset],
    ];
    for (const [x, y] of positions) { const box = { x: x!, y: y!, width, height }; if (!overlaps(box, rect)) return box; }
  }
  return null;
}
