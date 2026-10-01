/**
 * 설정 검색 — 순수 함수(DOM·React 없음).
 *
 * 질의를 공백으로 나눈 낱말이 **모두** 들어 있는 항목만 고른다(AND · 부분 일치 · 대소문자 무시).
 * 낱말은 제목 · 설명 · 동의어(keywords) · 범주 이름 어디에 있어도 되고, 띄어쓰기를 뺀 글에서도 찾는다
 * ("다시설치" → "다시 설치"). 한글 초성 검색은 하지 않는다.
 */

export interface SearchableEntry {
  id: string;
  cat: string;
  title: string;
  desc: string;
  keywords: readonly string[];
}

export interface Segment {
  text: string;
  hit: boolean;
}

export interface SearchHit<E extends SearchableEntry> {
  entry: E;
  score: number;
  title: Segment[];
  desc: Segment[];
  /** 제목·설명에는 없고 동의어로만 맞은 낱말의 그 동의어들(화면에 "관련어"로) */
  via: string[];
}

export interface SearchGroup<E extends SearchableEntry> {
  cat: string;
  hits: SearchHit<E>[];
}

function norm(s: string): string {
  return s.normalize("NFC").toLowerCase();
}

function squash(s: string): string {
  return s.replace(/\s+/g, "");
}

/** 질의 → 낱말(정규화·중복 제거). 빈 질의는 빈 배열 */
export function tokenize(query: string): string[] {
  const out: string[] = [];
  for (const t of norm(query).split(/\s+/)) {
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

function has(field: string, token: string): boolean {
  const f = norm(field);
  return f.includes(token) || squash(f).includes(token);
}

/** text 를 낱말이 나온 자리마다 hit 조각으로 나눈다(겹치는 자리는 합친다) */
export function highlight(text: string, tokens: readonly string[]): Segment[] {
  const lower = norm(text);
  // 대소문자 변환으로 길이가 바뀌는 글(드묾)은 위치가 어긋나므로 강조하지 않는다
  if (lower.length !== text.length || tokens.length === 0) return text ? [{ text, hit: false }] : [];
  const ranges: [number, number][] = [];
  for (const t of tokens) {
    let i = lower.indexOf(t);
    while (i !== -1) {
      ranges.push([i, i + t.length]);
      i = lower.indexOf(t, i + t.length);
    }
  }
  if (ranges.length === 0) return text ? [{ text, hit: false }] : [];
  ranges.sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([r[0], r[1]]);
  }
  const out: Segment[] = [];
  let at = 0;
  for (const [s, e] of merged) {
    if (s > at) out.push({ text: text.slice(at, s), hit: false });
    out.push({ text: text.slice(s, e), hit: true });
    at = e;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}

/** 한 항목 판정 — 하나라도 안 맞는 낱말이 있으면 null */
export function matchEntry<E extends SearchableEntry>(entry: E, tokens: readonly string[], catLabel = ""): SearchHit<E> | null {
  if (tokens.length === 0) return null;
  let score = 0;
  const via: string[] = [];
  for (const t of tokens) {
    const title = norm(entry.title);
    let s = 0;
    if (title.startsWith(t)) s = 4;
    else if (has(entry.title, t)) s = 3;
    const kw = entry.keywords.filter((k) => has(k, t));
    if (!s && kw.length) s = 2;
    if (!s && has(entry.desc, t)) s = 1;
    if (!s && catLabel && has(catLabel, t)) s = 0.5;
    if (!s) return null;
    score += s;
    if (!has(entry.title, t) && !has(entry.desc, t)) {
      for (const k of kw) if (!via.includes(k)) via.push(k);
    }
  }
  return { entry, score, title: highlight(entry.title, tokens), desc: highlight(entry.desc, tokens), via };
}

/**
 * 검색 — 범주별로 묶어 돌려준다. 묶음은 가장 잘 맞은 항목 점수가 높은 범주부터(같으면 catOrder 순),
 * 묶음 안은 점수 높은 순(같으면 정의 순서).
 */
export function searchSettings<E extends SearchableEntry>(
  entries: readonly E[],
  query: string,
  catOrder: readonly string[] = [],
  catLabels: Readonly<Record<string, string>> = {},
): SearchGroup<E>[] {
  const tokens = tokenize(query);
  if (tokens.length === 0) return [];
  const byCat = new Map<string, SearchHit<E>[]>();
  for (const e of entries) {
    const h = matchEntry(e, tokens, catLabels[e.cat] ?? "");
    if (!h) continue;
    const list = byCat.get(e.cat) ?? [];
    list.push(h);
    byCat.set(e.cat, list);
  }
  const rank = (c: string) => {
    const i = catOrder.indexOf(c);
    return i === -1 ? catOrder.length : i;
  };
  const groups = [...byCat.entries()].map(([cat, hits]) => ({
    cat,
    // 안정 정렬 — 같은 점수면 정의 순서
    hits: hits.map((h, i) => ({ h, i })).sort((a, b) => b.h.score - a.h.score || a.i - b.i).map((x) => x.h),
  }));
  groups.sort((a, b) => b.hits[0].score - a.hits[0].score || rank(a.cat) - rank(b.cat));
  return groups;
}

/** 묶음을 화면 순서대로 편 목록(화살표·엔터 이동용) */
export function flatHits<E extends SearchableEntry>(groups: readonly SearchGroup<E>[]): SearchHit<E>[] {
  return groups.flatMap((g) => g.hits);
}
