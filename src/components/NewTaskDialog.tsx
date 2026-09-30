import { useEffect, useRef, useState } from "react";
import { ImagePlus, X } from "lucide-react";
import { AGENT_LABEL, api, type Agent } from "../api";
import { kbd } from "../keys";
import { DraftTray, MAX_ATTS, hasFiles, imagesFromClipboard, useAttachDraft } from "./Attachments";

interface Props {
  toast: (m: string) => void;
  onClose: () => void;
  /** 띄운 세션 — 목록에 나타나면 연다 */
  onStarted: (shortId: string, sessionId: string | null) => void;
}

const AGENT_KEY = "ai-inbox.new-task-agent";

function lastAgent(): Agent {
  try {
    return localStorage.getItem(AGENT_KEY) === "codex" ? "codex" : "claude";
  } catch {
    return "claude";
  }
}

/** 새 작업: 폴더를 고르고 할 일을 적으면 그 폴더에서 Claude Code 나 Codex 를 백그라운드로 띄운다 */
export function NewTaskDialog({ onClose, onStarted, toast }: Props) {
  const [agent, setAgent] = useState<Agent>(lastAgent);
  /** codex 실행 파일 — undefined 확인 중 · null 없음 */
  const [codexBin, setCodexBin] = useState<string | null | undefined>(undefined);
  const [dirs, setDirs] = useState<string[]>([]);
  const [dir, setDir] = useState("");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const att = useAttachDraft("__new_task__", toast);

  useEffect(() => {
    api
      .recentDirs()
      .then((d) => {
        setDirs(d);
        setDir((cur) => cur || d[0] || "");
      })
      .catch(() => setDirs([]));
    api
      .codexStatus()
      .then((c) => setCodexBin(c.bin))
      .catch(() => setCodexBin(null));
    area.current?.focus();
  }, []);

  const pick = (a: Agent) => {
    setAgent(a);
    try {
      localStorage.setItem(AGENT_KEY, a);
    } catch {
      /* 저장소를 못 쓰면 이번만 */
    }
  };
  const codexMissing = agent === "codex" && codexBin === null;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  const ready =
    !!dir && (text.trim().length > 0 || att.ids.length > 0) && !att.uploading && !att.failed && !busy && !codexMissing && !(agent === "codex" && codexBin === undefined);
  const start = async () => {
    if (!ready) return;
    setBusy(true);
    setErr(null);
    try {
      const r = await api.startTask(dir, text.trim(), att.ids, agent);
      att.clear();
      onStarted(r.short_id, r.session_id);
    } catch (e) {
      setErr(String(e));
      setBusy(false);
    }
  };

  const options = dir && !dirs.includes(dir) ? [dir, ...dirs] : dirs;

  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="modal new-task">
        <header className="modal-head">
          <h2>새 작업</h2>
          <button className="icon-btn" title="닫기" onClick={onClose} disabled={busy}>
            <X size={18} />
          </button>
        </header>
        <section className="set">
          <h3>에이전트</h3>
          <div className="set-row">
            <div className="seg" role="radiogroup" aria-label="에이전트">
              {(["claude", "codex"] as Agent[]).map((a) => (
                <button key={a} role="radio" aria-checked={agent === a} className={agent === a ? "on" : ""} onClick={() => pick(a)} disabled={busy}>
                  {AGENT_LABEL[a]}
                </button>
              ))}
            </div>
            {codexMissing && <span className="set-note small inline">codex 실행 파일을 찾지 못했습니다 — Codex CLI 를 설치한 뒤 다시 여세요</span>}
          </div>
        </section>
        <section className="set">
          <h3>폴더</h3>
          <div className="set-row">
            <select className="dir-select" value={dir} onChange={(e) => setDir(e.target.value)} title={dir}>
              {options.length === 0 && <option value="">최근 폴더 없음 — 오른쪽에서 고르세요</option>}
              {options.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
            <button
              className="btn"
              onClick={async () => {
                const p = await api.pickFolder();
                if (p) setDir(p);
              }}
            >
              다른 폴더…
            </button>
          </div>
        </section>
        <section
          className="set"
          onDragOver={(e) => hasFiles(e) && e.preventDefault()}
          onDrop={(e) => {
            if (!hasFiles(e)) return;
            e.preventDefault();
            const files = imagesFromClipboard(e.dataTransfer);
            if (files.length) att.add(files);
            else toast("이미지 파일만 붙일 수 있습니다");
          }}
        >
          <h3>할 일</h3>
          <textarea
            ref={area}
            className="task-text"
            rows={6}
            maxLength={4000}
            value={text}
            spellCheck={false}
            placeholder={agent === "codex" ? "이 폴더에서 Codex 에게 시킬 일" : "이 폴더에서 Claude 에게 시킬 일"}
            onChange={(e) => setText(e.target.value)}
            onPaste={(e) => {
              const files = imagesFromClipboard(e.clipboardData);
              if (!files.length) return;
              e.preventDefault();
              att.add(files);
            }}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                start();
              }
            }}
          />
          <DraftTray items={att.items} onRemove={att.remove} />
          <div className="set-row">
            <button className="btn" disabled={att.items.length >= MAX_ATTS} onClick={() => picker.current?.click()}>
              <ImagePlus size={14} /> 이미지 붙이기
            </button>
            <span className="set-note small inline">붙여넣기({kbd("⌘V")})·끌어다 놓기도 됩니다 · {MAX_ATTS}장까지</span>
            <input
              ref={picker}
              type="file"
              accept="image/png,image/jpeg,image/gif,image/webp,image/heic,image/heif,image/tiff,image/bmp"
              multiple
              hidden
              onChange={(e) => {
                const files = Array.from(e.target.files ?? []);
                e.target.value = "";
                if (files.length) att.add(files);
              }}
            />
          </div>
          {agent === "codex" ? (
            <p className="set-note small">
              이 폴더에서 Codex 를 백그라운드로 실행합니다(codex exec). Codex 설정에 샌드박스를 정해 두지 않았으면 이 폴더 안에서만 파일을
              고칠 수 있게(workspace-write) 띄우고, 승인은 묻지 않습니다 — 막히는 일은 실패로 끝나니 채팅 위의 "이어가기"로 터미널에서 이어 가세요.
            </p>
          ) : (
            <p className="set-note small">
              이 폴더에서 Claude Code 를 백그라운드로 실행합니다. 권한은 평소 Claude Code 기본 설정 그대로이고, 승인이 필요하면 세션이
              멈춥니다 — 채팅 위의 "터미널에서 열기"로 열어 승인하세요.
            </p>
          )}
          {err && <p className="set-err">{err}</p>}
          <div className="set-row end">
            <button className="btn" onClick={onClose} disabled={busy}>
              취소
            </button>
            <button className="btn primary" disabled={!ready} onClick={start} title={`시작 (${kbd("⌘Enter")})`}>
              {busy ? "시작하는 중…" : "시작"}
            </button>
          </div>
        </section>
      </div>
    </div>
  );
}
