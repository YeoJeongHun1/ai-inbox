import { useCallback, useEffect, useRef, useState } from "react";
import { Archive, CheckCheck, Loader2, Pin, PinOff, Plus, Search, Settings, Smartphone, Trash2 } from "lucide-react";
import type { Counts, Filter, SessionItem } from "../api";
import { listTime } from "../format";

interface Props {
  sessions: SessionItem[];
  filter: Filter;
  onFilter: (f: Filter) => void;
  query: string;
  onQuery: (q: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
  counts: Counts;
  working: boolean;
  hookLine: { ok: boolean; text: string };
  onSettings: () => void;
  /** 폰 연결 상태(연결된 폰 수 · 지금 보고 있는 폰 수) — 누르면 QR 창 */
  phone: { devices: number; online: number } | null;
  onPhone: () => void;
  onNewTask: () => void;
  /** 기록 — 지난 대화 검색 · 보낸 메시지 · 이미지 */
  onArchive: () => void;
  onReadAll: () => void;
  /** 세션 줄 오른쪽 클릭 메뉴 */
  onSessionAction: (id: string, action: SessionAction) => void;
  /** 목록 아래에 띄울 알림(새 버전 등) */
  banner?: React.ReactNode;
  searchRef: React.RefObject<HTMLInputElement | null>;
}

export type SessionAction = "pin" | "unpin" | "read" | "archive" | "delete";

/** 세션 줄 오른쪽 클릭 메뉴 — 지우기는 한 번 더 눌러야 한다 */
function SessionMenu({ s, x, y, onPick, onClose }: { s: SessionItem; x: number; y: number; onPick: (a: SessionAction) => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [armed, setArmed] = useState(false);
  const [pos, setPos] = useState({ x, y });
  useEffect(() => {
    // 창 밖으로 나가지 않게
    const el = ref.current;
    if (el) {
      const r = el.getBoundingClientRect();
      setPos({ x: Math.min(x, window.innerWidth - r.width - 8), y: Math.min(y, window.innerHeight - r.height - 8) });
    }
    const close = (e: Event) => {
      if (e instanceof KeyboardEvent && e.key !== "Escape") return;
      if (e.type === "mousedown" && ref.current?.contains(e.target as Node)) return;
      onClose();
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, [x, y, onClose]);
  const pick = (a: SessionAction) => {
    onPick(a);
    onClose();
  };
  return (
    <div ref={ref} className="ctx-menu" role="menu" style={{ left: pos.x, top: pos.y }} onContextMenu={(e) => e.preventDefault()}>
      <button role="menuitem" onClick={() => pick(s.pinned ? "unpin" : "pin")}>
        {s.pinned ? <PinOff size={14} /> : <Pin size={14} />} {s.pinned ? "고정 해제" : "목록 위에 고정"}
      </button>
      <button role="menuitem" disabled={!s.unread} onClick={() => pick("read")}>
        <CheckCheck size={14} /> 모두 읽음
      </button>
      <hr />
      <button role="menuitem" onClick={() => pick("archive")} title="목록과 폰에서 빼고 기록 검색에는 남긴다 — 보관한 뒤 새 요청·결과가 오면 저절로 돌아온다">
        <Archive size={14} /> 보관
      </button>
      <button
        role="menuitem"
        className={armed ? "danger" : ""}
        disabled={s.active > 0}
        title={s.active > 0 ? "작업 중인 세션은 끝난 뒤에 지울 수 있습니다" : "이 앱에 모은 요청·보낸 메시지·이미지를 지운다 — Claude Code·Codex 원본 기록은 그대로"}
        onClick={() => (armed ? pick("delete") : setArmed(true))}
      >
        <Trash2 size={14} /> {armed ? "정말 지우기" : "기록에서 지우기…"}
      </button>
    </div>
  );
}

const TABS: { key: Filter; label: string; count?: keyof Counts }[] = [
  { key: "all", label: "전체" },
  { key: "unread", label: "안 읽음", count: "unread" },
  { key: "attention", label: "확인 필요", count: "attention" },
  { key: "active", label: "진행 중", count: "active" },
];

function initials(name: string): string {
  // dev-02 → D2, api-01-login → A1
  const m = name.match(/^([A-Za-z])[A-Za-z]*[-_ ]0*(\d+)/);
  if (m) return (m[1] + m[2]).toUpperCase().slice(0, 3);
  const clean = name.replace(/[^\p{L}\p{N}]+/gu, " ").trim();
  if (!clean) return "·";
  const words = clean.split(/\s+/);
  if (/^[\p{Script=Hangul}]/u.test(clean)) return clean.slice(0, 2);
  if (words.length > 1) return (words[0][0] + words[1][0]).toUpperCase();
  return clean.slice(0, 2).toUpperCase();
}

export function Sidebar(p: Props) {
  const [menu, setMenu] = useState<{ s: SessionItem; x: number; y: number } | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  return (
    <aside className="sidebar">
      <header className="side-head" data-tauri-drag-region>
        <span className="wordmark">AI Inbox</span>
        <div className="side-actions">
          <button className="phone-btn new-btn" title="폴더를 골라 Claude Code·Codex 에 새 일을 시키기 (⌘N)" onClick={p.onNewTask}>
            <Plus size={15} />
            새 작업
          </button>
          <button
            className={`phone-btn ${p.phone && p.phone.devices > 0 ? "paired" : ""}`}
            title="코노티 앱(폰)에서 이 PC 의 세션을 보고 답하기 — QR 로 연결"
            onClick={p.onPhone}
          >
            <Smartphone size={15} />
            {p.phone && p.phone.devices > 0 ? (
              <>
                폰 {p.phone.devices}
                {p.phone.online > 0 && <span className="phone-live" />}
              </>
            ) : (
              "폰 연결"
            )}
          </button>
          <button className="icon-btn" title="기록 — 지난 대화 검색·보낸 메시지·이미지 (⌘⇧F)" onClick={p.onArchive}>
            <Archive size={17} />
          </button>
          <button className="icon-btn" title="모든 세션 모두 읽음 — 되돌릴 수 있습니다" onClick={p.onReadAll} disabled={!p.counts.unread}>
            <CheckCheck size={17} />
          </button>
          <button className="icon-btn" title="설정" onClick={p.onSettings}>
            <Settings size={17} />
          </button>
        </div>
      </header>

      <label className="search">
        <Search size={15} />
        <input
          ref={p.searchRef}
          value={p.query}
          placeholder="세션 이름·요청·응답 검색"
          onChange={(e) => p.onQuery(e.target.value)}
          spellCheck={false}
        />
      </label>

      <nav className="tabs">
        {TABS.map((t) => {
          const n = t.count ? p.counts[t.count] : 0;
          return (
            <button key={t.key} className={`tab ${p.filter === t.key ? "on" : ""}`} onClick={() => p.onFilter(t.key)}>
              {t.label}
              {t.count && n > 0 && <span className={`tab-n ${t.key === "active" ? "plain" : ""}`}>{n}</span>}
            </button>
          );
        })}
      </nav>

      {p.filter === "unread" && p.counts.unread > 0 && !p.query && (
        <div className="list-bar">
          <span>
            안 읽은 결과 <strong>{p.counts.unread}</strong>
          </span>
          <button className="list-bar-btn" onClick={p.onReadAll} title="모든 세션의 안 읽은 결과를 한 번에 읽음으로 — 되돌릴 수 있습니다">
            <CheckCheck size={14} /> 모두 읽음
          </button>
        </div>
      )}

      <ul className="session-list">
        {p.sessions.map((s) => {
          const live = s.live_status === "busy";
          const mine = !s.last_from_ai && s.last_origin !== "peer";
          return (
            <li
              key={s.id}
              className={`session ${p.selectedId === s.id ? "on" : ""} ${s.unread ? "has-unread" : ""}`}
              onClick={() => p.onSelect(s.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenu({ s, x: e.clientX, y: e.clientY });
              }}
            >
              <div className={`avatar ${s.live_status ? "alive" : ""}`}>
                {initials(s.name)}
                {s.live_status && <span className={`live-dot ${live ? "busy" : ""}`} />}
              </div>
              <div className="session-body">
                <div className="row1">
                  <span className={`s-name ${s.named ? "" : "unnamed"}`}>{s.name}</span>
                  {s.agent === "codex" && <span className="s-agent" title="OpenAI Codex 세션">Codex</span>}
                  {s.project_name && <span className="s-proj">{s.project_name}</span>}
                  {s.pinned && <Pin size={12} className="s-pin" />}
                  <time className="s-time">{listTime(s.last_at)}</time>
                </div>
                <div className="row2">
                  <span className="s-preview">
                    {s.active > 0 && s.last_status !== "done" ? (
                      <span className="s-working">
                        <Loader2 size={12} className="spin" />
                        {s.last_status === "waiting" ? "승인·답변 대기" : s.last_status === "background" ? "백그라운드 작업 대기" : "작업 중"}
                        {" · "}
                      </span>
                    ) : null}
                    {mine && <span className="s-me">나: </span>}
                    {s.last_preview || "—"}
                  </span>
                  {s.attention > 0 && <span className="badge ask">?</span>}
                  {s.unread > 0 && <span className="badge">{s.unread}</span>}
                </div>
              </div>
            </li>
          );
        })}
        {p.sessions.length === 0 && (
          <li className="empty-list">
            {p.query ? "검색 결과가 없습니다." : p.filter === "all" ? "아직 모은 요청이 없습니다." : "해당하는 세션이 없습니다."}
          </li>
        )}
      </ul>

      {menu && (
        <SessionMenu
          key={`${menu.s.id}:${menu.x}:${menu.y}`}
          s={menu.s}
          x={menu.x}
          y={menu.y}
          onPick={(a) => p.onSessionAction(menu.s.id, a)}
          onClose={closeMenu}
        />
      )}
      {p.banner}
      <footer className="side-foot">
        <span className={`hook-dot ${p.hookLine.ok ? "ok" : ""}`} />
        <span className="hook-text">{p.hookLine.text}</span>
        {p.working && <span className="collecting">수집 중</span>}
      </footer>
    </aside>
  );
}
