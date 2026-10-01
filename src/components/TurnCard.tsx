// 턴 카드 — 요청 하나를 질문 · 이해 · 결과 · 응답 필요 · 과정 · 비용 순서로.
// 대화의 결과 말풍선(AiBubble)은 2~6칸을, 문서 패널은 6칸 전부를 이 조각들로 그린다. 규칙은 src/turncard.ts.

import { useLayoutEffect, useRef, useState } from "react";
import { AlertTriangle, Check, ChevronDown, ChevronRight, Circle, CircleHelp, CircleSlash, PauseCircle } from "lucide-react";
import { api, isFinished, phoneReply, quoteLabel, splitQuote, type Turn, type TurnDetail } from "../api";
import { clock, duration, fullTime, modelName, statusView, tildePath, tokens, toolName } from "../format";
import { parsePlan, shortPath } from "../markdown";
import {
  attentionOf,
  costParts,
  firstParagraph,
  oneLine,
  processItems,
  processSummary,
  SECTION,
  subagentLine,
  understandingOf,
  type Attention,
} from "../turncard";
import { AttStrip } from "./Attachments";
import { Markdown } from "./Markdown";

/** 4. 응답 필요 — 카드 맨 위 배지. 확정은 진하게, 추정은 점선·흐리게 */
export function AttentionBadge({ a }: { a: Attention }) {
  const Icon = a.sure ? (a.label === "권한 승인 대기" ? PauseCircle : CircleHelp) : CircleHelp;
  return (
    <div className={`tc-attn ${a.sure ? "sure" : "guess"}`} title={a.sure ? "세션이 답을 기다립니다" : "응답 끝이 물음으로 끝나 답이 필요해 보입니다(추정)"}>
      <Icon size={14} />
      <div className="tc-attn-main">
        <span className="tc-attn-label">
          {SECTION.attention} · {a.label}
          {!a.sure && <span className="tc-guess">추정</span>}
        </span>
        {a.items.length > 0 && (
          <ul className="tc-attn-items">
            {a.items.slice(0, 4).map((q, i) => (
              <li key={i}>{oneLine(q, 240)}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

/** 2. 이해 — 한 줄. 착수 멘트·응답 되풀이면 그리지 않는다 */
export function UnderstandingLine({ t }: { t: Pick<Turn, "understanding" | "response_text"> }) {
  const u = understandingOf(t.understanding, t.response_text);
  if (!u) return null;
  return (
    <div className="tc-under" title={u}>
      <span className="tc-label">{SECTION.understanding}</span>
      <span className="tc-under-text">{oneLine(u)}</span>
    </div>
  );
}

/** 칸을 넘칠 때만 아래를 흐리게 */
function Clamp({ children }: { children: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [over, setOver] = useState(false);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => setOver(el.scrollHeight > el.clientHeight + 2);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [children]);
  return (
    <div ref={ref} className={`ai-body ${over ? "over" : ""}`}>
      {children}
    </div>
  );
}

const CHAT_CAP = 6000;

/** 3. 결과 — 요약 한 줄 + 첫 단락, "더 보기"로 전체. cap: 대화 말풍선은 길이를 자른다(전체는 문서에) */
export function ResultBlock({ t, cap }: { t: Turn; cap?: boolean }) {
  const [more, setMore] = useState(false);
  const live = t.status === "running" || t.status === "background" || t.status === "waiting";
  const body = t.response_text?.trim() ?? "";
  const { head, more: hasMore } = firstParagraph(body);
  const full = cap && body.length > CHAT_CAP ? body.slice(0, CHAT_CAP) + "\n\n…(전체는 문서로 보기)" : body;
  return (
    <>
      {t.summary && <p className="ai-summary">{t.summary}</p>}
      {body ? (
        more ? (
          <div className="ai-body full">
            <Markdown>{full}</Markdown>
          </div>
        ) : (
          <Clamp>
            <Markdown>{head}</Markdown>
          </Clamp>
        )
      ) : (
        !live && (
          <div className="ai-empty">
            {t.prompt_source === "mid-turn" ? "따로 답한 글 없이 앞 요청 안에서 이어졌습니다." : "응답 텍스트 없이 끝났습니다."}
          </div>
        )
      )}
      {body && hasMore && (
        <button className="more" onClick={() => setMore(!more)}>
          {more ? "접기" : "더 보기"}
        </button>
      )}
    </>
  );
}

/** 5. 과정 — 기본 접힘 + 요약 한 줄. 오류·중단은 접혀 있어도 배지. 상세가 없으면 펼칠 때 불러온다 */
export function ProcessBlock({ t, detail: given, startOpen }: { t: Turn; detail?: TurnDetail | null; startOpen?: boolean }) {
  const [open, setOpen] = useState(!!startOpen);
  const [loaded, setLoaded] = useState<TurnDetail | null>(null);
  const detail = given ?? loaded;
  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && !detail) api.getTurn(t.id).then(setLoaded).catch(() => {});
  };
  const summary = processSummary(
    {
      toolCalls: t.tool_calls,
      tools: detail?.tools,
      files: detail ? detail.files.length : t.files_changed,
      subagents: detail ? detail.subagents.length : t.subagent_count,
      errors: 0, // 오류는 배지로 따로
    },
    toolName,
  );
  const interrupted = t.status === "interrupted";
  return (
    <div className={`tc-proc ${open ? "open" : ""}`}>
      <button className="tc-proc-head" onClick={toggle} aria-expanded={open} title={open ? "과정 접기" : "과정 펼치기"}>
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <span className="tc-label">{SECTION.process}</span>
        <span className="tc-proc-sum">{summary}</span>
        {t.error_count > 0 && (
          <span className="tc-badge err">
            <AlertTriangle size={11} /> 오류 {t.error_count}
          </span>
        )}
        {interrupted && (
          <span className="tc-badge">
            <CircleSlash size={11} /> 중단
          </span>
        )}
      </button>
      {open && (detail ? <ProcessBody d={detail} /> : <div className="tc-proc-body tc-dim">불러오는 중…</div>)}
    </div>
  );
}

function ProcessBody({ d }: { d: TurnDetail }) {
  const t = d.turn;
  const plan = parsePlan(t.plan_json);
  const items = processItems(d.steps, t.response_text, understandingOf(t.understanding, t.response_text));
  const [allTexts, setAllTexts] = useState<Set<number>>(new Set());
  return (
    <div className="tc-proc-body">
      {plan.length > 0 && (
        <div className="tc-sub">
          <div className="tc-sub-h">계획</div>
          <ul className="tc-list">
            {plan.map((p, i) => (
              <li key={i} className={p.status === "completed" ? "tc-done" : ""}>
                {p.status === "completed" ? <Check size={11} /> : <Circle size={9} />}
                {p.text}
              </li>
            ))}
          </ul>
        </div>
      )}
      {d.files.length > 0 && (
        <div className="tc-sub">
          <div className="tc-sub-h">바뀐 파일 {d.files.length}</div>
          <ul className="tc-list mono">
            {d.files.map(([p, n]) => (
              <li key={p}>
                {shortPath(p, t.cwd)}
                {n > 1 && <span className="tc-dim"> ({n}번 수정)</span>}
              </li>
            ))}
          </ul>
        </div>
      )}
      {d.subagents.length > 0 && (
        <div className="tc-sub">
          <div className="tc-sub-h">서브에이전트 {d.subagents.length}</div>
          <ul className="tc-list">
            {d.subagents.map((a, i) => (
              <li key={i}>{subagentLine(a, duration)}</li>
            ))}
          </ul>
        </div>
      )}
      {items.length > 0 && (
        <div className="tc-sub">
          <div className="tc-sub-h">시간순</div>
          <ol className="tc-steps">
            {items.map((it, i) => (
              <li key={i} className={it.kind === "error" ? "tc-warn err" : it.kind === "interrupt" ? "tc-warn" : ""}>
                <time>{clock(it.at)}</time>
                {it.kind === "tools" ? (
                  <span className="tc-step-text">
                    <code>{toolName(it.name)}</code>
                    {it.count > 1 && <b> ×{it.count}</b>}{" "}
                    {it.count === 1 ? (
                      it.texts[0] ?? ""
                    ) : (
                      <>
                        {(allTexts.has(i) ? it.texts : it.texts.slice(0, 3)).map((x, k) => (
                          <span key={k} className="tc-step-sub">
                            {x}
                          </span>
                        ))}
                        {it.texts.length > 3 && !allTexts.has(i) && (
                          <button className="more" onClick={() => setAllTexts(new Set(allTexts).add(i))}>
                            외 {it.texts.length - 3}개
                          </button>
                        )}
                      </>
                    )}
                  </span>
                ) : (
                  <span className="tc-step-text">{stepText(it.kind, it.name, it.text)}</span>
                )}
              </li>
            ))}
          </ol>
        </div>
      )}
      {!plan.length && !d.files.length && !d.subagents.length && !items.length && <div className="tc-dim">기록된 과정이 없습니다.</div>}
    </div>
  );
}

function stepText(kind: string, name: string | null, text: string): string {
  switch (kind) {
    case "task":
      return `백그라운드 알림 — ${text}`;
    case "ask":
      return `${name ? `${name} 이 보냄` : "작업 중에 받은 말"} — ${oneLine(text, 400)}`;
    case "summary":
      return `요약 — ${text}`;
    case "error":
      return `오류 — ${text}`;
    case "interrupt":
      return "사용자가 중단함";
    case "compact":
      return "대화 압축";
    default:
      return text.length > 600 ? text.slice(0, 600) + "…" : text;
  }
}

/** 6. 비용 — 모델 · 토큰(입력/출력, 캐시 따로) · 작업 시간 · 도구 호출 수. 달러 없음 */
export function CostLine({ t }: { t: Turn }) {
  const parts = costParts(t, { model: modelName, tokens, duration });
  return (
    <span className="tc-cost" title={SECTION.cost}>
      {parts.map((p, i) => (
        <span key={i}>{p}</span>
      ))}
    </span>
  );
}

/** 문서 패널의 카드 — 6칸 전부. 마크다운 복사·저장·폰 문서와 같은 순서 */
export function TurnCard({ detail, onOpenImages }: { detail: TurnDetail; onOpenImages: (ids: string[], index: number) => void }) {
  const t = detail.turn;
  const s = detail.session;
  const st = statusView(t.status, t.needs_input, t.pending_bg);
  const att = attentionOf(t, detail.steps);
  const phone = phoneReply(t.prompt_text);
  const { quote, rest } = splitQuote((phone ? phone.body : t.prompt_text) ?? "");
  const text = rest.trim();
  const long = text.length > 600 || text.split("\n").length > 12;
  const [moreQ, setMoreQ] = useState(false);
  const branch = s.git_branch && s.git_branch !== "HEAD" ? s.git_branch : null;
  const origin =
    t.origin === "peer"
      ? `다른 세션 ${t.peer_name ?? "?"} 이 보냄`
      : t.origin === "inbox"
        ? "AI Inbox 앱에서 보냄"
        : t.origin === "sched"
          ? "AI Inbox 예약 전송"
          : phone
            ? `폰(코노티)에서 보낸 답${phone.title ? ` — 원래 요청: ${phone.title}` : ""}`
            : null;

  return (
    <article className={`tc-doc tone-${st.tone}`}>
      <header className="tc-doc-head">
        <div className="tc-doc-where">{[s.name, s.project_name, branch].filter(Boolean).join(" · ")}</div>
        <div className="tc-doc-meta">
          <span className="ai-status">{st.label}</span>
          <span>{fullTime(t.prompt_at)}</span>
          <span>{duration(t.duration_ms)}</span>
        </div>
      </header>

      {att && <AttentionBadge a={att} />}

      <section className="tc-sec">
        <div className="tc-label">{SECTION.prompt}</div>
        {(origin || t.prompt_source === "mid-turn") && (
          <div className="tc-dim tc-origin">{[origin, t.prompt_source === "mid-turn" ? "앞 요청이 진행되는 중에 보낸 말" : null].filter(Boolean).join(" · ")}</div>
        )}
        {quote && (
          <div className="quote-block">
            <span className="q-label">{quoteLabel(quote)}에 답장</span>
            <span className="q-text">{quote.text}</span>
          </div>
        )}
        {t.slash_command && <code className="slash">{t.slash_command}</code>}
        {text ? (
          <div className={`user-text tc-prompt ${long && !moreQ ? "clamp" : ""}`}>{text}</div>
        ) : (
          !t.slash_command && <div className="tc-dim">{t.atts?.length ? "(이미지만 보냄)" : "(본문 없음)"}</div>
        )}
        {long && (
          <button className="more" onClick={() => setMoreQ(!moreQ)}>
            {moreQ ? "접기" : "더 보기"}
          </button>
        )}
        <AttStrip ids={t.atts ?? []} onOpen={onOpenImages} />
      </section>

      {understandingOf(t.understanding, t.response_text) && (
        <section className="tc-sec tc-sec-line">
          <UnderstandingLine t={t} />
        </section>
      )}

      <section className="tc-sec">
        <div className="tc-label">{SECTION.result}</div>
        <ResultBlock t={t} />
        {!t.response_text?.trim() && t.status === "running" && <div className="ai-empty">아직 작업 중</div>}
      </section>

      <section className="tc-sec tc-sec-line">
        <ProcessBlock t={t} detail={detail} />
      </section>

      <footer className="tc-sec tc-foot">
        <span className="tc-label">{SECTION.cost}</span>
        <CostLine t={t} />
      </footer>
      <div className="tc-where tc-dim">
        {t.ended_at && <span>끝 {fullTime(t.ended_at)}</span>}
        {t.cwd && <span>{tildePath(t.cwd)}</span>}
        <span>
          {s.name} ({s.id})
        </span>
      </div>

      <section className="tc-sec tc-next">
        <div className="tc-label">이어진 요청</div>
        {detail.next_id ? (
          <>
            <div className="tc-dim">{fullTime(detail.next_at)} · 같은 세션의 다음 요청</div>
            <div className="user-text clamp tc-next-text">{(detail.next_prompt ?? "").trim() || "(본문 없음)"}</div>
          </>
        ) : (
          <div className="tc-dim">{isFinished(t.status) ? "아직 이어진 요청이 없습니다." : "작업이 끝나면 다음 요청이 여기에 이어집니다."}</div>
        )}
      </section>
    </article>
  );
}
