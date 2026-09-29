import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { api, type UpdateState } from "../api";
import { Markdown } from "./Markdown";

interface BarProps {
  state: UpdateState | null;
  /** 폰 연결 서버가 이 버전을 더 받지 않을 때의 안내 */
  relayUpgrade: string | null;
  progress: { done: number; total: number | null } | null;
  onChanged: () => void;
  toast: (m: string) => void;
}

/** 사이드바 아래의 새 버전 알림 — 언제 바꿀지는 사용자가 고른다 */
export function UpdateBar({ state, relayUpgrade, progress, onChanged, toast }: BarProps) {
  const [later, setLater] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notes, setNotes] = useState(false);
  if (!state) return null;

  const install = async () => {
    setBusy(true);
    try {
      await api.updateInstall(); // 끝나면 앱이 다시 시작한다
    } catch (e) {
      toast(`업데이트하지 못했습니다 — ${e}`);
      setBusy(false);
    }
  };

  if (state.installed) {
    return (
      <div className="update-bar">
        <p>
          <strong>새 버전 {state.installed}</strong>이 설치됐습니다. 다시 시작하면 적용됩니다.
        </p>
        <div className="update-actions">
          <button className="btn primary" onClick={() => api.appRestart()}>
            다시 시작
          </button>
        </div>
      </div>
    );
  }

  const av = state.available;
  if (av && later !== av.version) {
    const pct = progress?.total ? Math.min(100, Math.round((progress.done / progress.total) * 100)) : null;
    return (
      <div className="update-bar">
        <p>
          <strong>새 버전 {av.version}</strong>이 있습니다{relayUpgrade ? " — 폰 연결을 계속 쓰려면 업데이트가 필요합니다" : ""}.
        </p>
        {notes && av.notes && (
          <div className="update-notes">
            <Markdown>{av.notes}</Markdown>
          </div>
        )}
        <div className="update-actions">
          <button className="btn primary" disabled={busy} onClick={install}>
            {busy ? (pct != null ? `받는 중 ${pct}%` : "받는 중…") : "업데이트"}
          </button>
          {av.notes && (
            <button className="btn" onClick={() => setNotes(!notes)}>
              {notes ? "접기" : "바뀐 점"}
            </button>
          )}
          {!busy && (
            <>
              <button className="more" onClick={() => setLater(av.version)}>
                나중에
              </button>
              <button
                className="more"
                onClick={async () => {
                  await api.updateSkip(av.version);
                  onChanged();
                }}
              >
                이 버전 건너뛰기
              </button>
            </>
          )}
        </div>
      </div>
    );
  }

  if (relayUpgrade) {
    return (
      <div className="update-bar">
        <p>{relayUpgrade}</p>
        <div className="update-actions">
          <button
            className="btn"
            onClick={async () => {
              try {
                const a = await api.updateCheckNow();
                if (!a) toast("받을 수 있는 새 버전을 아직 찾지 못했습니다");
              } catch (e) {
                toast(`새 버전을 확인하지 못했습니다 — ${e}`);
              }
              onChanged();
            }}
          >
            새 버전 확인
          </button>
        </div>
      </div>
    );
  }
  return null;
}

/** 업데이트된 뒤 처음 열 때 — 이번 버전에서 바뀐 점 */
export function WhatsNew({ version, notes, onClose }: { version: string; notes: string; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <header className="modal-head">
          <h2>AI Inbox {version}로 업데이트됐습니다</h2>
          <button className="icon-btn" title="닫기" onClick={onClose}>
            <X size={18} />
          </button>
        </header>
        <section className="set whats-new">
          <Markdown>{notes}</Markdown>
          <div className="set-row end">
            <button className="btn primary" onClick={onClose}>
              확인
            </button>
          </div>
        </section>
      </div>
    </div>
  );
}
