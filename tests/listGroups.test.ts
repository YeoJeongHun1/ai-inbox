// 세션 목록 묶음(고정 · 오늘 · 어제 · 지난 7일 · 이전)
import { test } from "node:test";
import assert from "node:assert/strict";
import { groupOf, groupSessions } from "../src/listGroups.ts";

const now = new Date(2026, 9, 1, 15, 0, 0); // 2026-10-01 15:00 (지역 시각)
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m, d, h).toISOString();

test("날짜 경계 — 자정 기준, 고정이 먼저", () => {
  assert.equal(groupOf({ pinned: false, last_at: at(2026, 9, 1, 0) }, now), "today");
  assert.equal(groupOf({ pinned: false, last_at: at(2026, 8, 30, 23) }, now), "yesterday");
  assert.equal(groupOf({ pinned: false, last_at: at(2026, 8, 25, 1) }, now), "week");
  assert.equal(groupOf({ pinned: false, last_at: at(2026, 8, 24, 23) }, now), "older");
  assert.equal(groupOf({ pinned: true, last_at: at(2025, 0, 1) }, now), "pinned");
  assert.equal(groupOf({ pinned: false, last_at: null }, now), "older");
  assert.equal(groupOf({ pinned: false, last_at: "not a date" }, now), "older");
});

test("묶음 순서는 고정 → 오늘 → … 이고, 묶음 안 순서는 들어온 그대로", () => {
  const list = [
    { id: "a", pinned: true, last_at: at(2026, 8, 1) },
    { id: "b", pinned: false, last_at: at(2026, 9, 1, 14) },
    { id: "c", pinned: false, last_at: at(2026, 8, 20) },
    { id: "d", pinned: false, last_at: at(2026, 9, 1, 9) },
    { id: "e", pinned: false, last_at: at(2026, 8, 30) },
  ];
  const g = groupSessions(list, now);
  assert.deepEqual(
    g.map((x) => [x.key, x.items.map((i) => i.id).join("")]),
    [
      ["pinned", "a"],
      ["today", "bd"],
      ["yesterday", "e"],
      ["older", "c"],
    ],
  );
  assert.deepEqual(groupSessions([], now), []);
});
