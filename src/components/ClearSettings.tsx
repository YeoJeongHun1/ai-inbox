import { useEffect, useState } from "react";
import { enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { api, POLICY_LABEL, type ClearOverview, type CliInfo, type HistoryStatus, type SchedPolicy, type SchedSettings } from "../api";
import { fullTime } from "../format";
import { kbd } from "../keys";
import { ClearDialog } from "./ClearDialog";
import { Note } from "./Note";

/** 설정 — /clear 로 끝난 대화의 기본 처리 */
export function ClearSection({ toast }: { toast: (m: string) => void }) {
  const [ov, setOv] = useState<ClearOverview | null>(null);
  const [dialog, setDialog] = useState(false);
  const load = () => {
    api.clearOverview().then(setOv).catch(() => setOv(null));
  };
  useEffect(load, []);
  const value = ov ? (ov.default_state === "ask" ? 0 : ov.default_state === "keep" ? 2 : 1) : 1;
  return (
    <section className="set">
      <h3>/clear 로 끝난 대화</h3>
      <Note>
        연결된 세션에서 /clear 하면 그 대화는 끝난 대화로 흐리게 표시됩니다. Claude Code 는 /clear 한 대화를 <code>/resume</code>·<code>/rewind</code> 로 되돌릴 수 있게
        {ov ? ` ${ov.retention_days}일` : " 30일"}(cleanupPeriodDays) 동안 두고 지웁니다. 삭제 예약한 대화는 그 기간 + 유예 {ov?.grace_days ?? 7}일이 지난 뒤에만 이 앱에서도 지워집니다.
      </Note>
      <label className="set-line">
        <span>새로 /clear 된 대화는</span>
        <select
          value={value}
          disabled={!ov}
          onChange={async (e) => {
            await api.setSetting("clear_default", Number(e.target.value));
            toast("기본 처리를 바꿨습니다 — 이미 표시된 대화는 그대로입니다");
            load();
          }}
        >
          <option value={1}>삭제 예약 (기본) — 알림에서 이력 보관으로 바꿀 수 있음</option>
          <option value={2}>이력으로 보관 — 자동으로 지우지 않음</option>
          <option value={0}>매번 묻기 — 정할 때까지 자동 삭제 없음</option>
        </select>
      </label>
      {ov && (
        <p className="set-note small">
          삭제 예약 {ov.purge}개 · 이력 보관 {ov.keep}개 · 처리 미정 {ov.ask}개 · 지금까지 지운 대화 {ov.purged_sessions}개
          {ov.next_purge_at ? ` · 가장 이른 삭제 예정 ${fullTime(ov.next_purge_at).slice(0, 10)}` : ""}
          <br />
          별표·고정한 요청이 있거나, 진행 중이거나, /clear 뒤 이어서 쓴 흔적이 있으면 지우지 않고 미룹니다. 지우기 전에는 건수만 기록합니다(내용 없음).
        </p>
      )}
      <div className="set-row" data-set-id="clear-tidy">
        <button className="btn" onClick={() => setDialog(true)}>
          끝난 대화 정리…
        </button>
      </div>
      {dialog && <ClearDialog onlyUndecided={false} onClose={() => setDialog(false)} onChanged={load} toast={toast} />}
    </section>
  );
}

const CLAUDE_MODELS = ["claude-haiku-4-5-20251001", "haiku", "sonnet", "opus"];
const CODEX_MODELS = ["gpt-6-luna", "gpt-6-sol", "gpt-6-astra"];

function cliLine(name: string, c: CliInfo | undefined, detecting: boolean): string {
  if (!c) return detecting ? `${name} — 확인 중…` : `${name} — 아직 확인하지 않았습니다`;
  if (!c.installed) return `${name} — 설치되어 있지 않습니다`;
  const v = c.version ? ` v${c.version}` : "";
  if (c.logged_in === false) return `${name}${v} — 설치됨 · 로그인이 필요합니다`;
  return `${name}${v} — 설치됨 · ${c.logged_in ? `로그인됨${c.login_kind ? `(${c.login_kind})` : ""}` : "로그인 상태를 확인하지 못했지만 시도는 해 봅니다"}`;
}

/** 설정 — 대화 이력 검색(모델 호출: 이 컴퓨터의 구독 CLI) */
export function HistorySection({ toast }: { toast: (m: string) => void }) {
  const [st, setSt] = useState<HistoryStatus | null>(null);
  const [claudeModel, setClaudeModel] = useState("");
  const [codexModel, setCodexModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [detecting, setDetecting] = useState(false);
  const load = () =>
    api
      .historyStatus()
      .then((s) => {
        setSt(s);
        setClaudeModel(s.model_claude);
        setCodexModel(s.model_codex);
        return s;
      })
      .catch(() => setSt(null));
  const detect = () => {
    setDetecting(true);
    api
      .historyDetect()
      .then(() => load())
      .catch((e) => toast(String(e)))
      .finally(() => setDetecting(false));
  };
  useEffect(() => {
    load();
    detect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  if (!st) return null;
  const run = async (f: () => Promise<unknown>, ok: string) => {
    setBusy(true);
    try {
      await f();
      toast(ok);
      await load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  const det = st.detection;
  const usable = (c?: CliInfo) => !!c && c.installed && c.logged_in !== false;
  const noneUsable = !!det && !usable(det.claude) && !usable(det.codex);
  const apiKeyLogin = det?.codex.login_kind?.startsWith("API 키");
  return (
    <section className="set">
      <h3>대화 이력 검색 (구독 AI 사용)</h3>
      <p className="set-note">
        왼쪽 위 <strong>이력 찾기</strong>({kbd("⌘⇧H")})에서 "예전에 어떤 작업을 했더라"를 물어볼 수 있습니다. 질문과 맞는 요청·결과 <strong>발췌</strong>만 골라, 이 컴퓨터에 설치·로그인해 둔{" "}
        <strong>Claude Code</strong>(<code>claude -p</code>) 또는 <strong>Codex</strong>(<code>codex exec</code>)로 답을 만듭니다 — API 키는 필요 없습니다. 발췌는 그 구독 서비스의 서버(Claude → Anthropic · Codex → OpenAI)로 나가고,
        <strong> 질문 한 번마다 구독 사용량이 소모됩니다.</strong> 모델은 빈 임시 폴더에서 도구 없이 한 번만 돌며 파일·명령에 접근할 수 없고, 훅·MCP·개인 설정은 켜지 않습니다. 이 호출은 이 앱의 목록에 세션으로 나타나지 않습니다.
        비밀값은 저장할 때 이미 가려지지만 완전하지 않으니, 보내고 싶지 않으면 켜지 마세요. "모델 없이 찾기"는 켜지 않아도 되고 아무 데도 접속하지 않습니다.
      </p>
      <div data-set-id="history-service">
      <div className="set-line">
        <span>감지된 서비스</span>
        <button className="btn" disabled={busy || detecting} onClick={detect}>
          {detecting ? "확인 중…" : "다시 감지"}
        </button>
      </div>
      <ul className="set-note small">
        <li>{cliLine("Claude Code", det?.claude, detecting)}</li>
        <li>{cliLine("Codex", det?.codex, detecting)}</li>
      </ul>
      {noneUsable && (
        <p className="set-note small">
          쓸 수 있는 구독 CLI 가 없습니다 — Claude Code 또는 Codex CLI 를 설치하고 로그인하면 여기서 고를 수 있습니다. 그때까지는 <strong>모델 없이 찾기</strong>만 됩니다.
        </p>
      )}
      {apiKeyLogin && <p className="set-note small">Codex 가 API 키로 로그인되어 있습니다 — 이 경우 구독이 아니라 API 사용료가 청구됩니다.</p>}
      <div className="set-line">
        <span>사용할 서비스</span>
        <select
          value={st.provider}
          disabled={busy}
          onChange={(e) => run(() => api.historySet("provider", e.target.value), "바꿨습니다")}
        >
          <option value="auto">자동(로그인된 것 · Claude 먼저)</option>
          <option value="claude" disabled={!!det && !usable(det.claude)}>Claude Code(구독)</option>
          <option value="codex" disabled={!!det && !usable(det.codex)}>Codex(ChatGPT 구독)</option>
        </select>
        <span className="set-note small">{st.active ? `지금은 ${st.active === "claude" ? "Claude Code" : "Codex"} 를 씁니다` : "쓸 수 있는 서비스 없음"}</span>
      </div>
      </div>
      <div data-set-id="history-model">
      <div className="set-line">
        <span>Claude 모델</span>
        <input className="text-in" list="llm-claude-models" value={claudeModel} spellCheck={false} onChange={(e) => setClaudeModel(e.target.value)} />
        <datalist id="llm-claude-models">
          {CLAUDE_MODELS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
        <button className="btn" disabled={busy || claudeModel.trim() === st.model_claude} onClick={() => run(() => api.historySet("model_claude", claudeModel.trim()), "모델을 바꿨습니다")}>
          바꾸기
        </button>
      </div>
      <div className="set-line">
        <span>Codex 모델</span>
        <input className="text-in" list="llm-codex-models" value={codexModel} spellCheck={false} onChange={(e) => setCodexModel(e.target.value)} />
        <datalist id="llm-codex-models">
          {CODEX_MODELS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
        <button className="btn" disabled={busy || codexModel.trim() === st.model_codex} onClick={() => run(() => api.historySet("model_codex", codexModel.trim()), "모델을 바꿨습니다")}>
          바꾸기
        </button>
      </div>
      </div>
      <div data-set-id="history-consent">
      <label className="check">
        <input
          type="checkbox"
          checked={st.consent}
          disabled={busy}
          onChange={(e) => run(() => api.historySet("consent", e.target.checked ? "1" : "0"), e.target.checked ? "동의했습니다" : "동의를 철회했습니다")}
        />
        대화 발췌(요청·결과 일부)가 선택한 구독 서비스(Anthropic 또는 OpenAI)의 서버로 전송되는 것과 구독 사용량이 소모되는 것에 동의합니다
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={st.enabled}
          disabled={busy || !st.consent}
          onChange={(e) => run(() => api.historySet("enabled", e.target.checked ? "1" : "0"), e.target.checked ? "이력 검색 모델을 켰습니다" : "껐습니다")}
        />
        모델로 답하기 켜기
      </label>
      <p className="set-note small">
        하루 {st.daily_cap}회까지 · 오늘 {st.calls_today}회 사용(요청 태그 제안과 함께 셉니다). 답이 늦거나 실패하면 자동으로 "모델 없이 찾기"로 보여 줍니다.
      </p>
      </div>
      {st.legacy_key && (
        <div className="set-line">
          <span className="set-note small">이전 버전이 저장한 API 키 파일이 남아 있습니다 — 더는 쓰지 않으니 지워도 됩니다.</span>
          <button className="btn" disabled={busy} onClick={() => run(() => api.historyForgetKey(), "키 파일을 지웠습니다")}>
            지우기
          </button>
        </div>
      )}
    </section>
  );
}

// ── 설정 — 예약 전송 ──────────────────────────────────────────────────────────

const DAYS = ["월", "화", "수", "목", "금", "토", "일"];

/** 설정 — 예약 전송: 바쁜 세션 정책 · 방해금지 창 · 예약 처리 정책(허용 세션 목록) · 멈춤 스위치 */
export function ScheduleSection({ toast }: { toast: (m: string) => void }) {
  const [st, setSt] = useState<SchedSettings | null>(null);
  const [sessions, setSessions] = useState<{ id: string; name: string }[]>([]);
  const [pick, setPick] = useState("");
  const [busy, setBusy] = useState(false);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const load = () => {
    api.schedSettings().then(setSt).catch(() => setSt(null));
    isEnabled().then(setAutostart).catch(() => setAutostart(null));
  };
  useEffect(() => {
    load();
    api
      .listSessions("all", "")
      .then((l) => setSessions(l.map((s) => ({ id: s.id, name: s.name }))))
      .catch(() => setSessions([]));
  }, []);
  if (!st) return null;
  const run = async (f: () => Promise<unknown>, ok?: string) => {
    setBusy(true);
    try {
      await f();
      if (ok) toast(ok);
      load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  const tz = (() => {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    } catch {
      return "UTC";
    }
  })();
  const w = st.windows[0];
  const saveWindow = (patch: Partial<{ days: number; start: string; end: string; enabled: boolean }>) =>
    run(() =>
      api.schedWindowSave({
        id: w?.id ?? null,
        name: w?.name ?? "방해금지",
        days: patch.days ?? w?.days ?? 127,
        start: patch.start ?? w?.start ?? "22:00",
        end: patch.end ?? w?.end ?? "08:00",
        tz: w?.tz ?? tz,
        enabled: patch.enabled ?? w?.enabled ?? true,
      }),
    );
  const notAllowed = sessions.filter((s) => !st.allow.some((a) => a.session_id === s.id));
  return (
    <section className="set">
      <h3>예약 전송</h3>
      <Note>
        입력창의 <strong>시계 버튼</strong>으로 쓴 말을 정한 시각(또는 N분 뒤)에 세션에 보냅니다. <strong>이 PC 의 AI Inbox 가 켜져 있을 때만</strong> 전달되고(서버에 저장하지 않습니다),
        세션이 꺼져 있거나 받을 수 없으면 자동으로 보내지 않고 <strong>알림만</strong> 갑니다.
      </Note>
      <label className="check">
        <input type="checkbox" checked={!st.paused} disabled={busy} onChange={(e) => run(() => api.schedSetSetting("paused", e.target.checked ? "0" : "1"), e.target.checked ? "예약 전송을 켰습니다" : "예약 전송을 멈췄습니다")} />
        예약 전송 켜기 (끄면 아무것도 발사하지 않고, 다시 켜면 놓친 예약은 각 예약의 "놓쳤을 때" 설정을 따릅니다)
      </label>
      {autostart === false && (
        <p className="set-err">
          로그인할 때 자동 실행이 꺼져 있습니다 — PC 를 다시 시작하면 앱이 꺼진 채라 예약이 전달되지 않습니다.{" "}
          <button className="more" onClick={async () => (await enable(), setAutostart(await isEnabled()))}>
            자동 실행 켜기
          </button>
        </p>
      )}
      <div className="set-line">
        <span>세션이 작업 중일 때 기본</span>
        <select value={st.busy_default} disabled={busy} onChange={(e) => run(() => api.schedSetSetting("busy_default", e.target.value), "바꿨습니다")}>
          {(Object.keys(POLICY_LABEL) as SchedPolicy[]).map((p) => (
            <option key={p} value={p}>
              {POLICY_LABEL[p]}
            </option>
          ))}
        </select>
      </div>
      <Note small>
        우선순위: 예약을 만들 때 고른 것 → 세션 규칙 → 태그 규칙 → 방해금지 시간대 → 이 기본값. 세션 규칙은 아래에서, 예약마다 덮어쓰기는 예약 창에서 정합니다.
      </Note>

      <div data-set-id="sched-dnd">
      <h4>방해금지 시간 (시간대 규칙)</h4>
      <div className="set-line">
        <label className="check">
          <input type="checkbox" checked={!!w?.enabled} disabled={busy} onChange={(e) => saveWindow({ enabled: e.target.checked })} /> 사용
        </label>
        <input className="text-in sched-time" type="time" value={w?.start ?? "22:00"} disabled={busy} onChange={(e) => saveWindow({ start: e.target.value })} />
        <span>~</span>
        <input className="text-in sched-time" type="time" value={w?.end ?? "08:00"} disabled={busy} onChange={(e) => saveWindow({ end: e.target.value })} />
        <span className="set-note small inline">{w?.tz ?? tz}</span>
      </div>
      <div className="set-line">
        {DAYS.map((d, i) => (
          <label key={d} className="check">
            <input type="checkbox" checked={((w?.days ?? 127) & (1 << i)) !== 0} disabled={busy} onChange={(e) => saveWindow({ days: e.target.checked ? (w?.days ?? 127) | (1 << i) : (w?.days ?? 127) & ~(1 << i) })} /> {d}
          </label>
        ))}
      </div>
      <Note small>이 시간 안에 시각이 된 예약은 "방해금지 시간이 끝난 뒤"로 미룹니다(예약·세션·태그 규칙이 따로 있으면 그쪽이 먼저). 자정을 넘겨도 됩니다.</Note>
      </div>

      <div data-set-id="sched-rules">
      {st.rules.length > 0 && (
        <>
          <h4>세션·태그 규칙</h4>
          <ul className="sched-ul">
            {st.rules.map((r) => (
              <li key={r.id} className="sched-row">
                <span>
                  {r.scope === "session" ? "세션" : "태그"} <strong>{r.label}</strong> — {POLICY_LABEL[r.action]}
                </span>
                <button className="btn" disabled={busy} onClick={() => run(() => api.schedRuleSet(r.scope, r.key, ""))}>
                  지우기
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="set-line">
        <span>세션 규칙 추가</span>
        <select value={pick} onChange={(e) => setPick(e.target.value)}>
          <option value="">세션 고르기…</option>
          {sessions.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </select>
        {(Object.keys(POLICY_LABEL) as SchedPolicy[]).map((p) => (
          <button key={p} className="btn" disabled={busy || !pick} onClick={() => run(() => api.schedRuleSet("session", pick, p), "규칙을 저장했습니다")}>
            {p === "interrupt" ? "바로" : p === "after_work" ? "작업 뒤" : "방해금지 뒤"}
          </button>
        ))}
      </div>
      </div>

      <div data-set-id="sched-perm">
      <h4>예약 처리 정책 (권한)</h4>
      <p className="set-note small">
        예약된 말은 그 세션이 <strong>지금 쓰는 권한 설정 그대로</strong> 실행됩니다 — 이미 떠 있는 세션에는 도구 허용 목록 같은 제한을 새로 걸 수 없습니다(세션을 시작할 때만 정해집니다).
        승인 대기에서 멈추면 10분 뒤 알림이 갑니다. 전부 허용 모드가 아닌 세션에 예약하면 만들 때 제약과 사전 준비 방법을 안내합니다(세션별로 끌 수 있음). 이 앱은 어떤 경우에도 권한을 넓히는 옵션을 붙이지 않습니다.
      </p>
      <div className="set-line">
        <label className="check">
          <input type="radio" checked={st.perm === "default"} disabled={busy} onChange={() => run(() => api.schedSetSetting("perm", "default"))} /> 기본 — 모든 세션에 예약할 수 있음(승인 대기 감지 알림)
        </label>
      </div>
      <div className="set-line">
        <label className="check">
          <input type="radio" checked={st.perm === "allowlist"} disabled={busy} onChange={() => run(() => api.schedSetSetting("perm", "allowlist"))} /> 허용 세션 목록 — 고른 세션에만 예약할 수 있음
        </label>
      </div>
      {st.perm === "allowlist" && (
        <>
          <ul className="sched-ul">
            {st.allow.length === 0 && <li className="set-note small">허용한 세션이 없습니다 — 지금은 어느 세션에도 예약할 수 없습니다.</li>}
            {st.allow.map((a) => (
              <li key={a.session_id} className="sched-row">
                <span>{a.name}</span>
                <button className="btn" disabled={busy} onClick={() => run(() => api.schedAllowSet(a.session_id, false))}>
                  빼기
                </button>
              </li>
            ))}
          </ul>
          <div className="set-line">
            <select value={pick} onChange={(e) => setPick(e.target.value)}>
              <option value="">허용할 세션 고르기…</option>
              {notAllowed.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
            <button className="btn" disabled={busy || !pick} onClick={() => run(() => api.schedAllowSet(pick, true), "허용했습니다")}>
              허용
            </button>
          </div>
        </>
      )}
      </div>
    </section>
  );
}
