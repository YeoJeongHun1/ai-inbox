// 세션 목록 묶음 — 고정 · 오늘 · 어제 · 이번 주 · 이전. 줄마다 붙던 고정 핀·날짜 정보를 묶음 머리 하나로 접는다.
// 서버 순서(고정 먼저 → 마지막 활동 최근 순)를 묶음 안에서 그대로 지킨다. 순서가 섞여 와도 머리가 두 번 나오지 않는다.

export type GroupKey = "pinned" | "today" | "yesterday" | "week" | "older";

export const GROUP_LABEL: Record<GroupKey, string> = {
  pinned: "고정",
  today: "오늘",
  yesterday: "어제",
  week: "지난 7일",
  older: "이전",
};

const ORDER: GroupKey[] = ["pinned", "today", "yesterday", "week", "older"];

const startOfDay = (t: Date) => new Date(t.getFullYear(), t.getMonth(), t.getDate()).getTime();

export function groupOf(item: { pinned: boolean; last_at: string | null }, now: Date = new Date()): GroupKey {
  if (item.pinned) return "pinned";
  if (!item.last_at) return "older";
  const t = new Date(item.last_at);
  if (Number.isNaN(t.getTime())) return "older";
  const today = startOfDay(now);
  const day = startOfDay(t);
  if (day >= today) return "today";
  if (day >= today - 86_400_000) return "yesterday";
  if (day >= today - 6 * 86_400_000) return "week";
  return "older";
}

export function groupSessions<T extends { pinned: boolean; last_at: string | null }>(list: T[], now: Date = new Date()): { key: GroupKey; label: string; items: T[] }[] {
  const by = new Map<GroupKey, T[]>();
  for (const s of list) {
    const k = groupOf(s, now);
    const arr = by.get(k);
    if (arr) arr.push(s);
    else by.set(k, [s]);
  }
  return ORDER.filter((k) => by.has(k)).map((k) => ({ key: k, label: GROUP_LABEL[k], items: by.get(k)! }));
}
