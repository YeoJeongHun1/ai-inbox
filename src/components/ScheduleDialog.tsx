import { useEffect, useMemo, useState } from "react";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { Clock, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, POLICY_LABEL, type PermInfo, type PermKind, type QuoteTarget, type SchedItem, type SchedMissed, type SchedPolicy, type SchedSettings, type SchedWhen } from "../api";

/** 브라우저(웹뷰)의 IANA 시간대 — 예약은 만든 곳의 시간대로 풀어 서머타임 경계를 맞춘다 */
export function localZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

const pad = (n: number) => String(n).padStart(2, "0");
/** datetime-local 입력값 형식(2026-10-01T09:00) */
function localStr(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** UTC ISO → 이 PC 의 datetime-local 값 */
export function isoToLocalStr(iso: string): string {
  return localStr(new Date(iso));
}

const FIRST_KEY = "ai-inbox.sched-autostart-asked";

function askedBefore(): boolean {
  try {
    return localStorage.getItem(FIRST_KEY) === "1";
  } catch {
    return false;
  }
}
function markAsked() {
  try {
    localStorage.setItem(FIRST_KEY, "1");
  } catch {
    /* 저장 실패는 무시 */
  }
}

const DOC_MODES = "https://code.claude.com/docs/en/permission-modes";
const DOC_PERMS = "https://code.claude.com/docs/en/permissions";

/** 모드별 현재 상태 한 줄 — 공식 문서(permission-modes)의 "무엇이 승인 없이 실행되는가" 표를 근거로 한다 */
const KIND_LINE: Record<PermKind, string> = {
  bypass: "",
  auto: "자동(auto) 모드 — 별도 검사 모델이 위험하다고 본 동작은 막히거나 멈출 수 있습니다.",
  accept_edits: "편집 자동 승인(acceptEdits) 모드 — 파일 편집과 기본적인 파일 명령만 승인 없이 실행됩니다. 그 밖의 셸 명령·네트워크는 승인을 기다립니다.",
  default: "수동(default) 모드 — 읽기만 승인 없이 실행되고, 편집·셸 명령은 매번 승인을 기다립니다.",
  plan: "계획(plan) 모드 — 읽기만 하고 파일을 고치지 않습니다. 예약된 말이 작업을 시키는 내용이면 계획만 세우고 멈춥니다.",
  dont_ask: "dontAsk 모드 — 미리 허용해 둔 도구만 실행되고, 승인이 필요한 동작은 물어보지 않고 거절됩니다.",
  unknown: "이 세션의 권한 모드를 확인하지 못했습니다(훅이 아직 기록을 남기지 않았거나 훅이 설치되어 있지 않습니다).",
};

/** 예약 시 권한 안내 — 전부 허용 모드가 아니면 제약과 사전 준비 방법을 알린다. 전부 허용이면 한 줄 고지만 */
function PermPanel({ info, onOff }: { info: PermInfo; onOff: () => void }) {
  const link = (url: string, label: string) => (
    <button type="button" className="more" onClick={() => openUrl(url).catch(() => undefined)}>
      {label}
    </button>
  );
  if (info.notice) {
    return (
      <section className="set">
        <h3>권한</h3>
        <p className="set-note small">승인 없이 실행되는 세션입니다 — 예약 내용은 신뢰하는 것만 넣으세요.</p>
      </section>
    );
  }
  if (!info.warn) return null;
  return (
    <section className="set sched-perm">
      <h3>이 세션은 전부 허용 모드가 아닙니다 — 예약 전에 준비하세요</h3>
      <p className="set-note small">{KIND_LINE[info.kind]}</p>
      <p className="set-note small">
        <strong>제약</strong>
      </p>
      <ul className="set-note small sched-perm-list">
        <li>승인이 필요한 도구 호출을 만나면 세션이 멈춥니다. 그러면 그 예약은 처리되지 않은 채 대기하고, 10분 뒤 "승인 필요" 알림이 가며 대기 상태로 남다가 7일 뒤 만료됩니다.</li>
        <li>AI Inbox 나 폰에서는 승인할 수 없습니다 — 그 세션의 터미널(또는 IDE)에서만 승인할 수 있습니다.</li>
        <li>권한 모드는 세션이 시작된 뒤에도 바뀔 수 있고, AI Inbox 는 마지막으로 받은 훅의 값만 압니다. 위 안내와 실제가 다를 수 있습니다.</li>
      </ul>
      <p className="set-note small">
        <strong>사전 세팅 안내</strong>
      </p>
      <ul className="set-note small sched-perm-list">
        <li>모드 바꾸기 — 그 세션 터미널에서 <code>Shift+Tab</code> 을 누르면 수동 → 편집 자동 승인 → 계획 순으로 돕니다. 자동(auto) 모드는 <code>--permission-mode auto</code> 로 시작한 세션에서 쓸 수 있고, 모든 확인을 끄는 <code>bypassPermissions</code> 는 시작할 때 <code>--dangerously-skip-permissions</code> 등으로 켠 세션에만 나타납니다(컨테이너·VM 같은 격리 환경에서만 권장).</li>
        <li>필요한 도구만 미리 허용 — 설정 파일의 <code>permissions.allow</code> 규칙(예: <code>Bash(npm test)</code>)에 넣어 두면 그 도구는 승인 없이 실행됩니다. 사용자 설정은 모든 세션에 함께 적용되니 범위를 좁게 정하세요.</li>
        <li>예약 내용이 어떤 도구를 쓸지(파일 편집 · 셸 명령 · 네트워크) 미리 확인하고, 위 방법 중 하나로 그 도구가 승인 없이 돌게 준비한 뒤 예약하세요.</li>
      </ul>
      <p className="set-note small">
        공식 문서: {link(DOC_MODES, "권한 모드")} · {link(DOC_PERMS, "권한 규칙")}
      </p>
      <label className="check">
        <input type="checkbox" onChange={onOff} /> 이 세션은 다시 안 보기
      </label>
    </section>
  );
}

interface Props {
  /** 새 예약(입력창의 글·이미지를 그대로) 또는 고치기(item) */
  sessionId: string;
  sessionName?: string;
  text?: string;
  /** 붙인 이미지 id(입력창에서 올려 둔 것) */
  attIds?: string[];
  quote?: QuoteTarget | null;
  item?: SchedItem;
  onClose: () => void;
  onDone: () => void;
  toast: (m: string) => void;
}

/** 예약 전송 — 정한 시각에 이 세션에 말을 넣는다. AI Inbox 가 켜져 있는 PC 에서만 돈다 */
export function ScheduleDialog({ sessionId, sessionName, text: initText, attIds: initAtts, quote, item, onClose, onDone, toast }: Props) {
  const editing = !!item;
  const [text, setText] = useState(item?.text ?? initText ?? "");
  const atts = item ? item.atts.map((a) => a.id) : initAtts ?? [];
  const [mode, setMode] = useState<"after" | "at">("after");
  const [afterMin, setAfterMin] = useState(30);
  const [local, setLocal] = useState(() => (item?.next_due_at ? isoToLocalStr(item.next_due_at) : localStr(new Date(Date.now() + 60 * 60_000))));
  const [missed, setMissed] = useState<SchedMissed>(item?.on_missed ?? "within");
  const [within, setWithin] = useState(item?.missed_within_min ?? 60);
  const [busy, setBusy] = useState<SchedPolicy | "">(item?.busy_policy ?? "");
  const [settings, setSettings] = useState<SchedSettings | null>(null);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [turnOn, setTurnOn] = useState(true);
  const [working, setWorking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [perm, setPerm] = useState<PermInfo | null>(null);

  useEffect(() => {
    api.schedPermInfo(sessionId).then(setPerm).catch(() => setPerm(null));
    api.schedSettings().then(setSettings).catch(() => setSettings(null));
    isEnabled().then(setAutostart).catch(() => setAutostart(null));
  }, []);
  useEffect(() => {
    if (item) setMode("at"); // 고칠 때는 정한 시각 그대로 보이게
  }, [item]);

  const now = Date.now();
  const preset = useMemo(() => {
    const d = new Date();
    const evening = new Date(d.getFullYear(), d.getMonth(), d.getDate(), 18, 0);
    const morning = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1, 9, 0);
    return { evening: evening.getTime() > now + 60_000 ? evening : null, morning };
  }, [now]);

  const when = (): SchedWhen => (mode === "after" ? { kind: "after", min: Math.max(1, Math.round(afterMin)) } : { kind: "at", local, tz: localZone() });
  const defaultLabel = settings ? POLICY_LABEL[settings.busy_default] : "작업이 끝난 뒤";
  const showAutostartAsk = autostart === false && !askedBefore();

  const submit = async () => {
    setWorking(true);
    setErr(null);
    try {
      const common = {
        text: text.trim(),
        atts,
        when: when(),
        on_missed: missed,
        missed_within_min: missed === "within" ? Math.round(within) : null,
        busy_policy: busy === "" ? null : busy,
      };
      const r = item
        ? await api.schedUpdate(item.id, { rev: item.rev, ...common })
        : await api.schedAdd({ session_id: sessionId, quote_turn: quote?.turn ?? null, quote_part: quote?.part ?? null, ...common });
      if (!editing && showAutostartAsk) {
        markAsked();
        if (turnOn) {
          try {
            await enable();
            toast("로그인할 때 자동으로 실행되도록 켰습니다");
          } catch (e) {
            toast(`자동 시작을 켜지 못했습니다: ${String(e)}`);
          }
        }
      }
      for (const w of r.warnings ?? []) toast(w);
      toast(editing ? "예약을 고쳤습니다" : "예약했습니다");
      onDone();
    } catch (e) {
      setErr(String(e));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal sched-dlg" role="dialog" aria-label="예약 전송">
        <header className="modal-head">
          <h2>
            <Clock size={16} /> {editing ? "예약 고치기" : "예약해서 보내기"}
          </h2>
          <button className="icon-btn" onClick={onClose} title="닫기">
            <X size={18} />
          </button>
        </header>
        <section className="set">
          <h3>{editing ? "보낼 말" : `보낼 말 — ${sessionName ?? "이 세션"}`}</h3>
          {editing ? (
            <textarea className="task-text" rows={4} maxLength={4000} value={text} spellCheck={false} onChange={(e) => setText(e.target.value)} />
          ) : (
            <p className="sched-preview">{text.trim() || (atts.length ? "(이미지만)" : "")}</p>
          )}
          {atts.length > 0 && <p className="set-note small">이미지 {atts.length}장 포함</p>}
          {quote && !editing && <p className="set-note small">요청 #{quote.seq} 에 대한 답장으로 보냅니다</p>}
        </section>

        <section className="set">
          <h3>언제</h3>
          <div className="set-row">
            {[30, 60, 180].map((m) => (
              <button key={m} className={`btn ${mode === "after" && afterMin === m ? "primary" : ""}`} onClick={() => (setMode("after"), setAfterMin(m))}>
                {m < 60 ? `${m}분 뒤` : `${m / 60}시간 뒤`}
              </button>
            ))}
            {preset.evening && (
              <button className="btn" onClick={() => (setMode("at"), setLocal(localStr(preset.evening!)))}>
                오늘 저녁 6시
              </button>
            )}
            <button className="btn" onClick={() => (setMode("at"), setLocal(localStr(preset.morning)))}>
              내일 아침 9시
            </button>
          </div>
          <div className="set-row">
            <label className="check">
              <input type="radio" checked={mode === "after"} onChange={() => setMode("after")} /> 지금부터
              <input className="text-in sched-num" type="number" min={1} max={43200} value={afterMin} onChange={(e) => (setMode("after"), setAfterMin(Number(e.target.value)))} /> 분 뒤
            </label>
          </div>
          <div className="set-row">
            <label className="check">
              <input type="radio" checked={mode === "at"} onChange={() => setMode("at")} /> 정한 시각
              <input className="text-in" type="datetime-local" value={local} onChange={(e) => (setMode("at"), setLocal(e.target.value))} />
            </label>
            <span className="set-note small inline">{localZone()}</span>
          </div>
        </section>

        <section className="set">
          <h3>PC 가 꺼져 있었거나 절전이라 시각을 놓쳤다면</h3>
          <div className="set-row">
            <select value={missed} onChange={(e) => setMissed(e.target.value as SchedMissed)}>
              <option value="within">늦어도 N분 안이면 실행</option>
              <option value="run_once">켜지는 대로 한 번 실행</option>
              <option value="skip">실행하지 않고 알림만(버리기)</option>
            </select>
            {missed === "within" && (
              <label className="check">
                <input className="text-in sched-num" type="number" min={1} max={1440} value={within} onChange={(e) => setWithin(Number(e.target.value))} /> 분
              </label>
            )}
          </div>
        </section>

        <section className="set">
          <h3>그 시각에 세션이 작업 중이라면</h3>
          <div className="set-row">
            <select value={busy} onChange={(e) => setBusy(e.target.value as SchedPolicy | "")}>
              <option value="">설정을 따름 (지금: {defaultLabel})</option>
              {(Object.keys(POLICY_LABEL) as SchedPolicy[]).map((p) => (
                <option key={p} value={p}>
                  {POLICY_LABEL[p]}
                </option>
              ))}
            </select>
          </div>
          <p className="set-note small">
            세션이 꺼져 있거나 넣을 수 없으면(또는 승인 대기에서 10분 넘게 멈추면) <strong>자동으로 보내지 않고 알림만</strong> 갑니다 — 세션이 다시 켜지면 한 번 더 알리고, 보낼지 버릴지 직접 고릅니다.
            예약된 말은 그 세션의 권한 설정 그대로 실행됩니다.
          </p>
        </section>

        {perm && (
          <PermPanel
            info={perm}
            onOff={() => {
              api
                .schedWarnOff(sessionId, true)
                .then(() => setPerm({ ...perm, warn: false, dismissed: true }))
                .catch((e) => toast(String(e)));
            }}
          />
        )}

        <section className="set">
          <h3>앱이 켜져 있어야 합니다</h3>
          <p className="set-note small">
            예약은 이 PC 의 AI Inbox 가 <strong>실행 중일 때만</strong> 전달됩니다. 앱이 꺼져 있거나 PC 가 잠들어 있으면 위의 "놓쳤다면" 설정을 따릅니다.
          </p>
          {autostart === false && (
            <>
              {showAutostartAsk ? (
                <label className="check">
                  <input type="checkbox" checked={turnOn} onChange={(e) => setTurnOn(e.target.checked)} /> 로그인할 때 자동으로 실행되게 켜기 (추천)
                </label>
              ) : (
                <p className="set-err">
                  자동 실행이 꺼져 있습니다 — PC 를 다시 시작하면 앱이 꺼진 채라 예약이 전달되지 않습니다.{" "}
                  <button
                    className="more"
                    onClick={async () => {
                      try {
                        await enable();
                        setAutostart(await isEnabled());
                      } catch (e) {
                        toast(String(e));
                      }
                    }}
                  >
                    자동 실행 켜기
                  </button>
                </p>
              )}
            </>
          )}
          {autostart === true && (
            <p className="set-note small">
              로그인할 때 자동 실행이 켜져 있습니다.{" "}
              <button className="more" onClick={async () => (await disable(), setAutostart(await isEnabled()))}>
                끄기
              </button>
            </p>
          )}
        </section>

        {err && <p className="set-err">{err}</p>}
        <div className="set-row end">
          <button className="btn" onClick={onClose} disabled={working}>
            취소
          </button>
          <button className="btn primary" onClick={submit} disabled={working || (!text.trim() && atts.length === 0)}>
            {working ? "저장하는 중…" : editing ? "저장" : "예약"}
          </button>
        </div>
      </div>
    </div>
  );
}
