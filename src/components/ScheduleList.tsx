import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Clock, X } from "lucide-react";
import { api, POLICY_LABEL, type SchedItem } from "../api";
import { fullTime } from "../format";
import { ScheduleDialog } from "./ScheduleDialog";

const MISSED_LABEL = { run_once: "놓치면 켜지는 대로 한 번 실행", skip: "놓치면 실행하지 않고 알림만", within: "놓쳐도 N분 안이면 실행" } as const;

/** 회차 상태 → 사람이 읽는 말 */
function runLabel(it: SchedItem): { text: string; tone: "" | "warn" | "ok" | "dim" } {
  const r = it.run;
  if (it.state === "cancelled") return { text: "취소됨", tone: "dim" };
  if (!r) return { text: "대기 중", tone: "" };
  switch (r.state) {
    case "pending":
    case "deferred":
      return { text: r.note ?? "넣을 때를 기다리는 중", tone: "" };
    case "fired":
      return { text: "세션에 넣는 중", tone: "" };
    case "delivered":
      return { text: "세션에 전달됨", tone: "ok" };
    case "handled":
      return { text: "전달됨 · 요청이 시작됨", tone: "ok" };
    case "held":
      return { text: `받지 못함 — ${r.note ?? "세션이 받을 수 없음"}`, tone: "warn" };
    case "missed":
      return { text: `놓침 — ${r.note ?? ""}`, tone: "warn" };
    case "failed":
      return { text: `막힘 — ${r.note ?? "설정상 보낼 수 없음"}`, tone: "warn" };
    case "cancelled":
      return { text: r.note ?? "취소됨", tone: "dim" };
    default:
      return { text: r.state, tone: "" };
  }
}

function rel(iso: string | null): string {
  if (!iso) return "";
  const m = Math.round((new Date(iso).getTime() - Date.now()) / 60_000);
  if (m <= 0) return "곧";
  if (m < 60) return `${m}분 뒤`;
  if (m < 60 * 24) return `${Math.floor(m / 60)}시간 ${m % 60}분 뒤`;
  return `${Math.floor(m / 1440)}일 뒤`;
}

interface Props {
  /** 있으면 그 세션의 예약만 */
  sessionId?: string;
  onClose: () => void;
  onOpenSession: (sid: string) => void;
  toast: (m: string) => void;
}

/** 예약 목록 — 대기 중인 예약·받지 못해 처리를 기다리는 예약(보내기/버리기)·최근 끝난 예약 */
export function ScheduleList({ sessionId, onClose, onOpenSession, toast }: Props) {
  const [items, setItems] = useState<SchedItem[] | null>(null);
  const [edit, setEdit] = useState<SchedItem | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(() => {
    api.schedList(sessionId).then(setItems).catch((e) => toast(String(e)));
  }, [sessionId, toast]);
  useEffect(() => {
    load();
    const un = listen("sched-changed", load);
    const t = setInterval(load, 30_000);
    return () => {
      clearInterval(t);
      un.then((f) => f());
    };
  }, [load]);
  // Esc 로 닫기 — 설정·기록 창과 같게. 고치기 창이 떠 있으면 그 창만 닫힌다(그쪽이 Esc 를 받는다)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !edit && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, edit]);

  const act = async (id: string, op: "send" | "drop") => {
    setBusy(id);
    try {
      await api.schedAct(id, op);
      toast(op === "send" ? "지금 보냈습니다" : "버렸습니다");
      load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  };
  const cancel = async (it: SchedItem) => {
    setBusy(it.id);
    try {
      await api.schedCancel(it.id);
      toast("예약을 취소했습니다");
      load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  };

  const held = (items ?? []).filter((i) => i.run?.state === "held");
  const waiting = (items ?? []).filter((i) => i.state === "active" && i.run?.state !== "held");
  const ended = (items ?? []).filter((i) => i.state !== "active");

  const row = (it: SchedItem, kind: "held" | "wait" | "end") => {
    const st = runLabel(it);
    return (
      <li key={it.id} className={`sched-row ${st.tone}`}>
        <div className="sched-main">
          <button className="more" title="이 세션으로 가기" onClick={() => (onOpenSession(it.session_id), onClose())}>
            {it.session_name}
          </button>
          <span className="sched-text">{it.text || "(이미지만)"}</span>
          <span className="set-note small inline">
            {it.next_due_at ? `${fullTime(it.next_due_at)} (${rel(it.next_due_at)})` : it.run ? `${fullTime(it.run.occurrence_at)} 예정이었음` : ""}
            {" · "}
            {st.text}
          </span>
          {kind === "wait" && (
            <span className="set-note small inline">
              {it.on_missed === "within" ? `놓쳐도 ${it.missed_within_min}분 안이면 실행` : MISSED_LABEL[it.on_missed]} · 작업 중이면 {it.busy_policy ? POLICY_LABEL[it.busy_policy] : "설정을 따름"}
            </span>
          )}
        </div>
        <div className="sched-btns">
          {kind === "held" && (
            <>
              <button className="btn primary" disabled={busy === it.id} onClick={() => act(it.id, "send")} title="PC 앞에서 직접 보내는 것과 같습니다 — 꺼진 세션이면 백그라운드로 이어서 실행합니다">
                지금 보내기
              </button>
              <button className="btn" disabled={busy === it.id} onClick={() => act(it.id, "drop")}>
                버리기
              </button>
            </>
          )}
          {kind === "wait" && it.next_due_at && (
            <button className="btn" disabled={busy === it.id} onClick={() => setEdit(it)}>
              고치기
            </button>
          )}
          {kind === "wait" && (
            <button className="btn" disabled={busy === it.id} onClick={() => cancel(it)}>
              취소
            </button>
          )}
        </div>
      </li>
    );
  };

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal sched-list" role="dialog" aria-label="예약 목록">
        <header className="modal-head">
          <h2>
            <Clock size={16} /> 예약 {sessionId ? "(이 세션)" : ""}
          </h2>
          <button className="icon-btn" onClick={onClose} title="닫기">
            <X size={18} />
          </button>
        </header>
        {items === null && <p className="set-note">불러오는 중…</p>}
        {items && items.length === 0 && (
          <p className="set-note">
            걸려 있는 예약이 없습니다. 세션 아래 입력창의 <Clock size={13} /> 버튼으로, 쓴 말을 정한 시각에 보내도록 예약할 수 있습니다. 예약은 이 PC 의 AI Inbox 가 켜져 있을 때만 전달됩니다.
          </p>
        )}
        {held.length > 0 && (
          <section className="set">
            <h3>처리가 필요합니다 ({held.length})</h3>
            <p className="set-note small">시각이 되었지만 세션이 받을 수 없어 자동으로 보내지 않았습니다. 세션이 다시 켜지면 알림이 한 번 더 갑니다 — 보낼지 버릴지 고르세요(7일 뒤 자동으로 버립니다).</p>
            <ul className="sched-ul">{held.map((i) => row(i, "held"))}</ul>
          </section>
        )}
        {waiting.length > 0 && (
          <section className="set">
            <h3>대기 중 ({waiting.length})</h3>
            <ul className="sched-ul">{waiting.map((i) => row(i, "wait"))}</ul>
          </section>
        )}
        {ended.length > 0 && (
          <section className="set">
            <h3>최근 끝난 예약 (7일)</h3>
            <ul className="sched-ul">{ended.map((i) => row(i, "end"))}</ul>
          </section>
        )}
      </div>
      {edit && (
        <ScheduleDialog
          sessionId={edit.session_id}
          sessionName={edit.session_name}
          item={edit}
          toast={toast}
          onClose={() => setEdit(null)}
          onDone={() => {
            setEdit(null);
            load();
          }}
        />
      )}
    </div>
  );
}
