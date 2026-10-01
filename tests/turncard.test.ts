// 턴 카드 규칙 시험 — `npm test` (Node 22.18+ 의 타입 지우기로 그대로 돈다, 의존성 없음)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  attentionOf,
  costParts,
  firstParagraph,
  processItems,
  processSummary,
  understandingOf,
} from "../src/turncard.ts";

const vectors = JSON.parse(readFileSync(new URL("../src-tauri/tests/fixtures/turn_card_vectors.json", import.meta.url), "utf8"));

test("이해 칸 거르기 — 공용 시험값(doc.rs 와 같은 답)", () => {
  for (const v of vectors.understanding) {
    assert.equal(understandingOf(v.u, v.r), v.out, JSON.stringify(v));
  }
});

test("결과 첫 단락 — 코드 블록 안 빈 줄·제목만 있는 단락", () => {
  assert.deepEqual(firstParagraph("첫 단락\n이어짐\n\n둘째"), { head: "첫 단락\n이어짐", more: true });
  assert.deepEqual(firstParagraph("## 결과\n\n본문\n\n끝"), { head: "## 결과\n\n본문", more: true });
  assert.deepEqual(firstParagraph("```\na\n\nb\n```\n\n뒤"), { head: "```\na\n\nb\n```", more: true });
  assert.deepEqual(firstParagraph("한 단락뿐"), { head: "한 단락뿐", more: false });
  assert.deepEqual(firstParagraph(null), { head: "", more: false });
});

const base = { response_text: null, needs_input: false, last_step: null } as const;

test("응답 필요 — 확정과 추정을 가른다", () => {
  assert.equal(attentionOf({ ...base, status: "done" }), null);
  const ask = attentionOf({ ...base, status: "waiting", last_step: ["tool", "AskUserQuestion", "어느 쪽으로 할까요?"] });
  assert.deepEqual(ask, { sure: true, label: "질문에 답 필요", items: ["어느 쪽으로 할까요?"] });
  assert.equal(attentionOf({ ...base, status: "waiting", last_step: ["text", null, "x"] })?.label, "승인·답변 대기");
  const steps = [{ seq: 1, at: null, kind: "tool" as const, name: "Bash", text: "rm -rf build" }];
  assert.equal(attentionOf({ ...base, status: "waiting" }, steps)?.label, "권한 승인 대기");
  const guess = attentionOf({ ...base, status: "done", needs_input: true, response_text: "고쳤습니다.\n- 배포할까요?" });
  assert.deepEqual(guess, { sure: false, label: "답이 필요해 보임", items: ["배포할까요?"] });
  // 작업 중에 다른 쪽이 보낸 말(ask 단계)은 응답 필요가 아니다
  assert.equal(attentionOf({ ...base, status: "done", last_step: ["ask", null, "x"] }), null);
});

test("과정 요약·묶음", () => {
  assert.equal(processSummary({ toolCalls: 0, files: 0, subagents: 0, errors: 0 }), "도구 없이 답함");
  assert.equal(
    processSummary({ toolCalls: 12, tools: [["Read", 4], ["Bash", 5], ["Edit", 2], ["Grep", 1]], files: 3, subagents: 1, errors: 1 }),
    "도구 12회(Bash 5, Read 4, Edit 2) · 파일 3 · 서브에이전트 1 · 오류 1",
  );
  assert.equal(processSummary({ toolCalls: 3, files: 0, subagents: 0, errors: 0 }), "도구 3회");
  const items = processItems(
    [
      { seq: 1, at: "a", kind: "text", name: null, text: "확인해 보겠습니다." },
      { seq: 2, at: "b", kind: "tool", name: "Read", text: "a.ts" },
      { seq: 3, at: "c", kind: "tool", name: "Read", text: "b.ts" },
      { seq: 4, at: "d", kind: "tool", name: "Bash", text: "ls" },
      { seq: 5, at: "e", kind: "error", name: null, text: "실패" },
      { seq: 6, at: "f", kind: "text", name: null, text: "끝났습니다." },
    ],
    "끝났습니다.",
    "확인해 보겠습니다.",
  );
  assert.deepEqual(
    items.map((i) => (i.kind === "tools" ? `${i.name}×${i.count}` : i.kind)),
    ["Read×2", "Bash×1", "error"],
  );
});

test("비용 한 줄 — 캐시는 따로, 달러 없음", () => {
  const f = { model: (m: string | null) => m ?? "—", tokens: (n: number) => String(n), duration: (ms: number) => `${ms / 1000}초` };
  const t = { model: "m", effort: "high", input_tokens: 10, output_tokens: 20, cache_read: 300, cache_create_5m: 4, cache_create_1h: 1, active_ms: 5000, tool_calls: 2 };
  assert.deepEqual(costParts(t, f), ["m · effort high", "입력 10 / 출력 20", "캐시 읽기 300 · 쓰기 5", "작업 5초", "도구 2회"]);
  assert.deepEqual(costParts({ ...t, model: null, effort: null, input_tokens: 0, output_tokens: 0, cache_read: 0, cache_create_5m: 0, cache_create_1h: 0, active_ms: null, tool_calls: 0 }, f), []);
  assert.ok(!costParts(t, f).join("").includes("$"));
});
