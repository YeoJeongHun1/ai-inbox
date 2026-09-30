import { useEffect, useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, filterActive, type TagFilter, type TagInfo, type TagOverview, type TurnTag } from "./api";

/** 태그 목록의 앱 전체 사본 — 백엔드가 "tags-changed" 를 알릴 때마다(배치 태깅 진행 포함) 다시 받는다 */
interface Snapshot {
  ov: TagOverview | null;
  byId: Map<number, TagInfo>;
}

let snap: Snapshot = { ov: null, byId: new Map() };
const subs = new Set<() => void>();
let started = false;
let timer: number | undefined;

function apply(ov: TagOverview) {
  if (snap.ov && JSON.stringify(snap.ov) === JSON.stringify(ov)) return;
  snap = { ov, byId: new Map(ov.tags.map((t) => [t.id, t])) };
  subs.forEach((f) => f());
}

/** 바로(또는 잠깐 모아서) 다시 읽는다 */
export function reloadTags(now = false) {
  window.clearTimeout(timer);
  const run = () => api.tagOverview().then(apply).catch(() => {});
  if (now) run();
  else timer = window.setTimeout(run, 300);
}

function start() {
  if (started) return;
  started = true;
  reloadTags(true);
  listen("tags-changed", () => reloadTags());
}

export function useTags(): Snapshot {
  useEffect(start, []);
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => snap,
  );
}

export const FALLBACK_COLOR = "#8a8a93";
export const tagColor = (t: TagInfo | undefined) => (t?.color && /^#[0-9a-f]{6}$/i.test(t.color) ? t.color : FALLBACK_COLOR);
/** 색을 고를 때 보여 주는 은은한 색 — 백엔드 `tags::PALETTE` 와 같은 값 */
export const PALETTE = ["#5b7c99", "#8a6f9e", "#6f9a7b", "#b0855a", "#a3606b", "#4f9a9a", "#8d8d5a", "#7a7fb5", "#b06a4a", "#6a6a6a"];

/** 화면에 내보일 표식: 모델 제안(ai)은 받아들이기 전이라 따로 다룬다 */
export const activeTags = (tags: TurnTag[] | undefined) => (tags ?? []).filter((t) => t.state !== "ai");

/** 백엔드 `TagFilter::sql` 과 같은 규칙 — 목차처럼 이미 받아 둔 항목을 화면에서 거를 때 */
export function matchFilter(tags: TurnTag[] | undefined, f: TagFilter): boolean {
  if (!filterActive(f)) return true;
  const on = activeTags(tags).map((t) => t.id);
  if (f.all && !f.untagged) return f.tags.every((id) => on.includes(id));
  if (f.untagged && on.length === 0) return true;
  return f.tags.some((id) => on.includes(id));
}

export function toggleTag(f: TagFilter, id: number): TagFilter {
  const has = f.tags.includes(id);
  const tags = has ? f.tags.filter((x) => x !== id) : [...f.tags, id];
  return { ...f, tags, all: tags.length < 2 ? false : f.all };
}
export const toggleUntagged = (f: TagFilter): TagFilter => ({ ...f, untagged: !f.untagged, all: false });

export function filterLabel(f: TagFilter, byId: Map<number, TagInfo>): string {
  const names = f.tags.map((id) => byId.get(id)?.name).filter(Boolean) as string[];
  if (f.untagged) names.push("미분류");
  return names.join(f.all ? " + " : " · ");
}
