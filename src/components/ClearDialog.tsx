import { useCallback, useEffect, useState } from "react";
import { Archive, Clock, X } from "lucide-react";
import { api, type ClearRow, type EndedState } from "../api";
import { endedLabel, fullTime } from "../format";

interface Props {
  /** true 면 아직 안내를 확인하지 않은 것만 보여 준다 */
  onlyUndecided: boolean;
  onClose: () => void;
  /** 처리를 바꿨다 — 목록·머리 표시를 다시 읽는다 */
  onChanged: () => void;
  toast: (m: string) => void;
}

/** /clear 로 끝난 대화의 처리를 고르는 창 — 이력으로 보관 · 삭제 예약 · 그대로 두기 */
export function ClearDialog({ onlyUndecided, onClose, onChanged, toast }: Props) {
  const [rows, setRows] = useState<ClearRow[] | null>(null);
  const [all, setAll] = useState(!onlyUndecided);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api.clearList(!all).then(setRows).catch((e) => toast(String(e)));
  }, [all, toast]);
  useEffect(load, [load]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const decide = async (ids: string[], d: EndedState, msg: string) => {
    if (!ids.length || busy) return;
    setBusy(true);
    try {
      await api.clearDecide(ids, d);
      toast(msg);
      onChanged();
      load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  const ack = async (ids: string[]) => {
    if (!ids.length || busy) return;
    setBusy(true);
    try {
      await api.clearAck(ids);
      toast("기본 처리를 그대로 둡니다");
      onChanged();
      load();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  const ids = (rows ?? []).map((r) => r.id);

  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="modal clear-dialog" onClick={(e) => e.stopPropagation()}>
        <header className="modal-head">
          <h2>/clear 로 끝난 대화</h2>
          <button className="icon-btn" onClick={onClose} title="닫기">
            <X size={18} />
          </button>
        </header>
        <p className="set-note clear-lead">
          Claude Code 에서 /clear 하면 그 세션은 끝난 대화가 됩니다. <strong>이력으로 보관</strong>하면 이 앱에 계속 남아 이력 탭·이력 찾기에서 볼 수 있고,
          <strong> 삭제 예약</strong>이면 Claude Code 에서 /resume·/rewind 로 되돌릴 수 있는 기간이 지난 뒤 이 앱의 사본도 지웁니다(원본 기록은 건드리지 않습니다).
        </p>
        <label className="check clear-all">
          <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} />
          이미 확인한 것까지 모두 보기
        </label>
        {rows && rows.length > 0 && (
          <div className="set-row wrap clear-bulk">
            <button className="btn primary" disabled={busy} onClick={() => decide(ids, "keep", `${ids.length}개를 이력으로 보관합니다`)}>
              <Archive size={14} /> 모두 이력으로 보관
            </button>
            <button className="btn" disabled={busy} onClick={() => decide(ids, "purge", `${ids.length}개를 삭제 예약합니다`)}>
              <Clock size={14} /> 모두 삭제 예약
            </button>
            {!all && (
              <button className="btn" disabled={busy} onClick={() => ack(ids)}>
                그대로 두기
              </button>
            )}
          </div>
        )}
        <ul className="clear-list">
          {(rows ?? []).map((r) => (
            <li key={r.id} className={`clear-row st-${r.ended.state}`}>
              <div className="clear-main">
                <span className="clear-name">{r.name}</span>
                <span className="clear-meta">
                  {r.project_name ? `${r.project_name} · ` : ""}요청 {r.turns}개 · {fullTime(r.last_at).slice(0, 16)}
                </span>
                <span className={`clear-state st-${r.ended.state}`}>{endedLabel(r.ended)}</span>
              </div>
              <div className="clear-btns">
                <button className={`btn small ${r.ended.state === "keep" ? "on" : ""}`} disabled={busy} onClick={() => decide([r.id], "keep", "이력으로 보관합니다")}>
                  보관
                </button>
                <button className={`btn small ${r.ended.state === "purge" ? "on" : ""}`} disabled={busy} onClick={() => decide([r.id], "purge", "삭제 예약했습니다")}>
                  삭제 예약
                </button>
              </div>
            </li>
          ))}
          {rows && rows.length === 0 && <li className="empty-list">{all ? "/clear 로 끝난 대화가 없습니다." : "결정할 대화가 없습니다."}</li>}
        </ul>
      </div>
    </div>
  );
}
