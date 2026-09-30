export const IS_MAC = navigator.userAgent.includes("Mac");

/** 단축키 표기 — macOS 는 그대로(⌘⇧F), 그 밖은 Ctrl+Shift+F. 처리기는 metaKey·ctrlKey 둘 다 받는다 */
export function kbd(keys: string): string {
  if (IS_MAC) return keys;
  return keys.replace(/⌘/g, "Ctrl+").replace(/⇧/g, "Shift+").replace(/⌥/g, "Alt+");
}
