import { useEffect, useMemo, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { ChevronDown, ChevronUp, Code2, Copy, Download, Eye, MailOpen, Star, X } from "lucide-react";
import { api, isFinished, type TurnDetail } from "../api";
import { kbd } from "../keys";
import { buildTurnMarkdown, suggestedFileName } from "../markdown";
import { Lightbox } from "./Attachments";
import { TurnCard } from "./TurnCard";
import { MoreMenu } from "./Menu";

interface Props {
  turnId: number;
  refreshKey: number;
  onClose: () => void;
  onNavigate: (id: number) => void;
  onChanged: () => void;
  toast: (msg: string) => void;
}

export function DocPanel({ turnId, refreshKey, onClose, onNavigate, onChanged, toast }: Props) {
  const [detail, setDetail] = useState<TurnDetail | null>(null);
  const [raw, setRaw] = useState(false);
  const [viewer, setViewer] = useState<{ ids: string[]; index: number } | null>(null);

  useEffect(() => {
    let alive = true;
    api.getTurn(turnId).then((d) => {
      if (!alive) return;
      setDetail(d);
      // 문서를 열었다 = 봤다
      if (isFinished(d.turn.status) && !d.turn.read_at) {
        api.markRead([turnId]).then(onChanged);
      }
    });
    return () => {
      alive = false;
    };
  }, [turnId, refreshKey, onChanged]);

  const md = useMemo(() => (detail ? buildTurnMarkdown(detail) : ""), [detail]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      if (viewer) return; // 이미지 보기가 먼저 Esc 를 받는다
      if (e.key === "Escape") onClose();
      if (!detail) return;
      if ((e.key === "k" || e.key === "ArrowUp") && e.altKey && detail.prev_id) onNavigate(detail.prev_id);
      if ((e.key === "j" || e.key === "ArrowDown") && e.altKey && detail.next_id) onNavigate(detail.next_id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [detail, viewer, onClose, onNavigate]);

  if (!detail) return <aside className="doc loading" />;
  const t = detail.turn;

  return (
    <aside className="doc">
      <header className="doc-head" data-tauri-drag-region>
        <div className="doc-nav">
          <button className="icon-btn" title={`이전 요청 (${kbd("⌥↑")})`} disabled={!detail.prev_id} onClick={() => detail.prev_id && onNavigate(detail.prev_id)}>
            <ChevronUp size={17} />
          </button>
          <button className="icon-btn" title={`다음 요청 (${kbd("⌥↓")})`} disabled={!detail.next_id} onClick={() => detail.next_id && onNavigate(detail.next_id)}>
            <ChevronDown size={17} />
          </button>
          <span className="doc-seq">요청 {t.seq}</span>
        </div>
        <div className="doc-actions">
          <button
            className={`icon-btn ${t.starred ? "on" : ""}`}
            title={t.starred ? "별표 해제" : "별표"}
            aria-pressed={t.starred}
            onClick={async () => {
              await api.setStarred(t.id, !t.starred);
              setDetail({ ...detail, turn: { ...t, starred: !t.starred } });
              onChanged();
            }}
          >
            <Star size={17} className={t.starred ? "filled" : ""} />
          </button>
          <MoreMenu
            title="더보기 — 원문 · 복사 · 저장 · 안 읽음"
            items={[
              { label: raw ? "문서로 보기" : "마크다운 원문 보기", icon: raw ? <Eye size={14} /> : <Code2 size={14} />, onClick: () => setRaw(!raw) },
              {
                label: "마크다운 복사",
                icon: <Copy size={14} />,
                onClick: async () => {
                  await writeText(md);
                  toast("마크다운을 복사했습니다");
                },
              },
              {
                label: ".md 파일로 저장",
                icon: <Download size={14} />,
                onClick: async () => {
                  try {
                    if (await api.saveMarkdown(suggestedFileName(detail), md)) toast("저장했습니다");
                  } catch (e) {
                    toast(`저장 실패: ${e}`);
                  }
                },
              },
              "sep",
              {
                label: "안 읽음으로 표시",
                icon: <MailOpen size={14} />,
                disabled: !t.read_at,
                onClick: async () => {
                  await api.markUnread(t.id);
                  onChanged();
                  onClose();
                },
              },
            ]}
          />
          <button className="icon-btn" title="닫기 (Esc)" onClick={onClose}>
            <X size={18} />
          </button>
        </div>
      </header>
      <div className="doc-scroll">
        {/* 화면은 턴 카드(접기·펼치기), 원문·복사·저장·폰은 같은 순서의 마크다운 */}
        {raw ? <pre className="doc-raw">{md}</pre> : <TurnCard detail={detail} onOpenImages={(ids, index) => setViewer({ ids, index })} />}
      </div>
      {viewer && <Lightbox ids={viewer.ids} index={viewer.index} onClose={() => setViewer(null)} toast={toast} />}
    </aside>
  );
}
