import { useEffect, useRef, useState } from "react";
import { ArrowUp, History, Loader2, Search, Settings as SettingsIcon } from "lucide-react";
import { api, filterActive, NO_FILTER, type TagFilter, type HistoryAnswer, type HistoryMsg, type HistorySource, type HistoryStatus } from "../api";
import { fullTime, modelName } from "../format";
import { Markdown } from "./Markdown";
import { TagFilterChips, TagLabels } from "./TagUi";

interface Turn {
  role: "user" | "assistant";
  content: string;
  /** 모델 없이 찾기만 한 답인가 */
  localOnly?: boolean;
  answer?: HistoryAnswer;
  error?: boolean;
}

interface Props {
  /** 근거 요청을 대화 창에서 연다 */
  onOpenTurn: (sessionId: string, turnId: number) => void;
  onSettings: () => void;
  toast: (m: string) => void;
}

const EXAMPLES = ["지난주에 무슨 작업을 했더라?", "결제 웹훅 관련해서 예전에 뭘 시켰지?", "어제 고친 버그가 뭐였어?"];

/** 저장된 대화 이력을 찾는 채팅 — Claude Code·Codex 세션에 지시하는 대화가 아니다(세션엔 아무것도 전달되지 않는다) */
export function HistoryChat({ onOpenTurn, onSettings, toast }: Props) {
  const [st, setSt] = useState<HistoryStatus | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [localOnly, setLocalOnly] = useState(false);
  /** 찾는 범위를 태그로 좁힌다 — 고른 태그가 붙은 요청 안에서만 찾고 답한다(태그 이름 자체는 모델에 보내지 않는다) */
  const [tagFilter, setTagFilter] = useState<TagFilter>(NO_FILTER);
  const scroller = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);

  const ready = !!st && st.enabled && st.consent && st.active !== null;
  useEffect(() => {
    // 구독 CLI 감지는 CLI 를 몇 번 부르므로 처음 열 때 한 번(결과는 앱이 기억한다)
    api
      .historyStatus()
      .then((s) => {
        setSt(s);
        if (!s.detection) api.historyDetect().then(() => api.historyStatus().then(setSt)).catch(() => {});
      })
      .catch(() => setSt(null));
  }, []);
  // 모델을 쓸 수 없으면 모델 없이 찾기로
  useEffect(() => {
    if (st && !ready) setLocalOnly(true);
  }, [st, ready]);
  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight, behavior: "smooth" });
  }, [turns, busy]);
  useEffect(() => {
    box.current?.focus();
  }, []);

  const send = async (q?: string) => {
    const content = (q ?? text).trim();
    if (!content || busy) return;
    setText("");
    const history: HistoryMsg[] = turns.filter((t) => !t.error && !t.localOnly).map((t) => ({ role: t.role, content: t.content }));
    const next: Turn[] = [...turns, { role: "user", content }];
    setTurns(next);
    setBusy(true);
    try {
      const a = await api.historyAsk([...history, { role: "user", content }], localOnly, tagFilter);
      setTurns([...next, { role: "assistant", content: a.text, answer: a, localOnly }]);
      api.historyStatus().then(setSt).catch(() => {});
    } catch (e) {
      setTurns([...next, { role: "assistant", content: String(e), error: true }]);
    } finally {
      setBusy(false);
      box.current?.focus();
    }
  };

  return (
    <section className="chat history-chat">
      <header className="chat-head" data-tauri-drag-region>
        <div className="chat-title">
          <h1>
            <History size={18} className="hc-icon" /> 대화 이력 찾기
          </h1>
          <div className="chat-sub">
            <span>내 작업 이력에서 찾는 대화 — Claude Code·Codex 세션에는 아무것도 전달되지 않습니다</span>
            {st && <span className="hc-model">{localOnly ? "모델 없이 찾기(외부 전송 없음)" : modelName(st.model)}</span>}
          </div>
        </div>
        <div className="chat-actions">
          <label className={`phone-mode ${localOnly ? "" : "on"}`} title={ready ? "모델로 답하려면 발췌가 구독 서비스(Claude·Codex)의 서버로 전송되고 구독 사용량이 소모됩니다" : "설정에서 켜고 동의하면 모델로 답할 수 있습니다"}>
            <select value={localOnly ? "local" : "model"} disabled={!ready} onChange={(e) => setLocalOnly(e.target.value === "local")}>
              <option value="model">모델로 답하기</option>
              <option value="local">모델 없이 찾기</option>
            </select>
          </label>
          <button className="text-btn" onClick={onSettings}>
            <SettingsIcon size={16} /> 설정
          </button>
        </div>
      </header>

      {st && !ready && (
        <div className="archived-bar hc-notice">
          <Search size={14} />
          <span>
            모델로 답하려면 설정의 "대화 이력 검색"에서 외부 전송에 동의하고, Claude Code 또는 Codex 를 설치·로그인해 두세요(API 키는 필요 없습니다). 지금은 <strong>모델 없이</strong> 이 앱에 저장된 요청·결과에서 키워드·날짜로 찾아 보여 줍니다.
          </span>
          <button className="more" onClick={onSettings}>
            설정 열기
          </button>
        </div>
      )}

      <TagFilterChips filter={tagFilter} onFilter={setTagFilter} className="hc-tags" />
      {filterActive(tagFilter) && (
        <div className="archived-bar hc-notice">
          <span>고른 태그가 붙은 요청 안에서만 찾습니다. 질문에 낱말이 없어도 그 태그의 최근 요청을 보여 줍니다.</span>
        </div>
      )}

      <div className="chat-scroll" ref={scroller}>
        <div className="chat-inner hc-inner">
          {turns.length === 0 && (
            <div className="hc-empty">
              <p>예전에 어떤 작업을 했는지 물어보세요. 지운 세션은 찾을 수 없고, /clear 로 끝난 대화는 삭제 예정 전까지 · 이력으로 보관한 대화는 계속 찾을 수 있습니다.</p>
              <div className="hc-examples">
                {EXAMPLES.map((e) => (
                  <button key={e} className="hc-chip" onClick={() => send(e)}>
                    {e}
                  </button>
                ))}
              </div>
            </div>
          )}
          {turns.map((t, i) =>
            t.role === "user" ? (
              <div key={i} className="user-row">
                <div className="user">
                  <div className="user-text">{t.content}</div>
                </div>
              </div>
            ) : (
              <div key={i} className={`ai hc-answer ${t.error ? "tone-bad" : ""}`}>
                {t.error ? (
                  <div className="set-err">{t.content}</div>
                ) : (
                  <>
                    {t.content ? (
                      <Markdown>{t.content}</Markdown>
                    ) : (
                      <p className="hc-plain">
                        {t.answer && t.answer.hits > 0
                          ? `질문과 맞는 기록 ${t.answer.hits}건을 찾았습니다(모델 없이 찾기).`
                          : "질문과 맞는 기록을 찾지 못했습니다. 다른 낱말이나 날짜(예: 지난주, 3일 전)로 물어보세요."}
                      </p>
                    )}
                    {t.answer && t.answer.sources.length > 0 && <Sources sources={t.answer.sources} onOpen={onOpenTurn} />}
                    {t.answer && (
                      <div className="hc-foot">
                        {t.answer.range && <span>범위 {t.answer.range[0] === t.answer.range[1] ? t.answer.range[0] : `${t.answer.range[0]} ~ ${t.answer.range[1]}`}</span>}
                        <span>찾은 기록 {t.answer.hits}건</span>
                        {t.answer.model && <span>{modelName(t.answer.model)}</span>}
                        <span>{(t.answer.ms / 1000).toFixed(1)}초</span>
                      </div>
                    )}
                  </>
                )}
              </div>
            ),
          )}
          {busy && (
            <div className="ai hc-answer">
              <span className="s-working">
                <Loader2 size={13} className="spin" /> 이력에서 찾는 중…
              </span>
            </div>
          )}
        </div>
      </div>

      <div className="composer">
        <div className="composer-box">
          <textarea
            ref={box}
            rows={1}
            value={text}
            maxLength={1000}
            placeholder="내 대화 이력에서 찾기 — 예: 지난주에 결제 관련해서 뭘 했지?"
            spellCheck={false}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send().catch((err) => toast(String(err)));
              }
            }}
          />
          <button className="send-btn" title="찾기 (Enter)" disabled={!text.trim() || busy} onClick={() => send()}>
            <ArrowUp size={17} />
          </button>
        </div>
        <div className="composer-hint">
          <span>이 창의 말은 세션에 전달되지 않습니다 · {localOnly ? "이 PC 안에서만 찾습니다" : "찾은 발췌만 모델에 보냅니다(도구·파일 접근 없음)"}</span>
        </div>
      </div>
    </section>
  );
}

function Sources({ sources, onOpen }: { sources: HistorySource[]; onOpen: (sid: string, turnId: number) => void }) {
  return (
    <ul className="hc-sources">
      {sources.map((s) => (
        <li key={`${s.turn_id}-${s.n}`} className={s.ended ? "ended" : ""}>
          <button onClick={() => onOpen(s.session_id, s.turn_id)} title="그 대화에서 열기">
            <span className="hc-n">{s.cited ? `[${s.n}]` : "·"}</span>
            <span className="hc-src-main">
              <span className="hc-src-title">
                {s.session_name} <em>#{s.seq}</em>
                {s.ended && <span className="hc-tag">{s.ended === "keep" ? "이력 보관" : "끝난 대화"}</span>}
                <TagLabels tags={s.tags ?? []} />
              </span>
              <span className="hc-src-sub">
                {fullTime(s.at).slice(0, 16)}
                {s.project ? ` · ${s.project}` : ""} — {s.prompt}
              </span>
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}
