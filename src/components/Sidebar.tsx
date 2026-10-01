import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Archive, CheckCheck, CircleHelp, Clock, History, Loader2, PauseCircle, Pin, PinOff, Plus, Search, Settings, Smartphone, Trash2 } from "lucide-react";
import type { Counts, Filter, SessionItem } from "../api";
import { endedLabel, listTime } from "../format";
import { kbd } from "../keys";
import { groupSessions } from "../listGroups";
import { tagColor, useTags } from "../tags";
import { AboutBadge } from "./About";

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
  /** 대화 이력 찾기(검색 모드 채팅) */
  onHistoryChat: () => void;
  historyChatOpen: boolean;
  /** 예약 전송: 걸려 있는 예약 수 · 받지 못해 처리를 기다리는 수 — 누르면 예약 목록 */
  sched: { active: number; held: number };
  onSched: () => void;
  /** /clear 로 끝난 대화의 결정 창 */
  onDecide: () => void;
  /** 세션 줄 오른쪽 클릭 메뉴 */
  onSessionAction: (id: string, action: SessionAction) => void;
  /** 목록 아래에 띄울 알림(새 버전 등) */
  banner?: React.ReactNode;
  searchRef: React.RefObject<HTMLInputElement | null>;
  toast: (m: string) => void;
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
      el.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
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
      {/* 줄에서 뺀 정보(폴더 · 태그)는 여기 첫머리에 읽기 전용으로 */}
      {(s.project_name || s.tags?.length) && (
        <div className="menu-info">
          <SessionFacts s={s} />
        </div>
      )}
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

/** 세션의 폴더 · 대표 태그 — 줄에는 상시로 두지 않고 메뉴·툴팁에서만 */
function SessionFacts({ s }: { s: SessionItem }) {
  const { byId } = useTags();
  const tags = (s.tags ?? []).filter((id) => byId.has(id));
  return (
    <>
      {s.project_name && <span className="mi-line">{s.project_name}</span>}
      {tags.length > 0 && (
        <span className="mi-line mi-tags">
          {tags.map((id) => (
            <span key={id} className="mi-tag">
              <span className="tag-dot" style={{ background: tagColor(byId.get(id)) }} />
              {byId.get(id)!.name}
            </span>
          ))}
        </span>
      )}
    </>
  );
}

const TABS: { key: Filter; label: string; count?: keyof Counts }[] = [
  { key: "all", label: "전체" },
  { key: "unread", label: "안 읽음", count: "unread" },
  { key: "attention", label: "확인 필요", count: "attention" },
  { key: "active", label: "진행 중", count: "active" },
  { key: "history", label: "이력", count: "kept" },
];

/** 줄 앞 상태 표식 — 모양이 다르다(색만으로 가르지 않는다): 작업 중 = 도는 원 · 승인 대기 = 멈춤 · 켜져 쉼 = 점 · 꺼짐 = 없음 */
function RowState({ s }: { s: SessionItem }) {
  const working = s.active > 0 && s.last_status !== "done";
  if (working && s.last_status === "waiting") return <PauseCircle size={13} className="rs rs-wait" aria-label="승인·답변 대기" />;
  if (working) return <Loader2 size={13} className="rs rs-busy spin" aria-label={s.last_status === "background" ? "백그라운드 작업 대기" : "작업 중"} />;
  if (s.live_status) return <span className="rs rs-idle" aria-label="열려 있음 · 입력 대기" />;
  return <span className="rs rs-off" aria-hidden />;
}

function rowTitle(s: SessionItem): string {
  const parts = [s.name];
  if (s.project_name) parts.push(`폴더 ${s.project_name}`);
  if (s.agent === "codex") parts.push("Codex 세션");
  if (s.active > 0 && s.last_status !== "done") parts.push(s.last_status === "waiting" ? "승인·답변 대기" : s.last_status === "background" ? "백그라운드 작업 대기" : "작업 중");
  else if (s.live_status) parts.push("열려 있음");
  if (s.ended) parts.push(endedLabel(s.ended));
  parts.push("오른쪽 클릭: 고정 · 모두 읽음 · 보관 · 지우기");
  return parts.join(" · ");
}

export function Sidebar(p: Props) {
  const [menu, setMenu] = useState<{ s: SessionItem; x: number; y: number } | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  const groups = useMemo(() => groupSessions(p.sessions), [p.sessions]);
  // 묶음 머리는 2묶음 이상일 때만(하나뿐이면 머리가 정보가 아니다)
  const showHeads = groups.length > 1 || groups[0]?.key === "pinned";
  return (
    <aside className="sidebar">
      <header className="side-head" data-tauri-drag-region>
        <span className="wordmark">AI Inbox</span>
        <button className="bar-btn new-btn" title={`새 작업 — 폴더를 골라 Claude Code·Codex 에 새 일을 시키기 (${kbd("⌘N")})`} aria-label="새 작업" onClick={p.onNewTask}>
          <Plus size={15} />
          <span className="btn-label">새 작업</span>
        </button>
      </header>

      <div className="search-row">
        <label className="search">
          <Search size={15} />
          <input
            ref={p.searchRef}
            value={p.query}
            placeholder="세션 검색"
            title={`세션 이름·요청·응답에서 찾기 (${kbd("⌘F")})`}
            onChange={(e) => p.onQuery(e.target.value)}
            spellCheck={false}
          />
          <button
            className={`icon-btn in-search ${p.historyChatOpen ? "on" : ""}`}
            title={`대화 이력 찾기 — 내 작업 이력에서 찾는 대화(세션에 지시하지 않음) (${kbd("⌘⇧H")})`}
            aria-pressed={p.historyChatOpen}
            onClick={(e) => {
              e.preventDefault();
              p.onHistoryChat();
            }}
          >
            <History size={15} />
          </button>
        </label>
      </div>

      <nav className="tabs" aria-label="세션 거르기">
        {TABS.map((t) => {
          const n = t.count ? p.counts[t.count] : 0;
          return (
            <button
              key={t.key}
              role="tab"
              aria-selected={p.filter === t.key}
              className={`tab ${p.filter === t.key ? "on" : ""}`}
              onClick={() => p.onFilter(t.key)}
              title={t.key === "history" ? "/clear 뒤 이력으로 보관한 대화" : undefined}
            >
              {t.label}
              {t.count && n > 0 && <span className="tab-n">{n}</span>}
            </button>
          );
        })}
      </nav>

      {p.counts.undecided > 0 && !p.query && (
        <div className="list-bar clear-bar">
          <span>
            /clear 된 대화 <strong>{p.counts.undecided}</strong>개
          </span>
          <button className="list-bar-btn" onClick={p.onDecide} title="/clear 로 끝난 대화를 이력으로 남길지 — 기본은 삭제 예약이고, 이력으로 보관하거나 그대로 둘 수 있습니다">
            정하기
          </button>
        </div>
      )}

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
        {groups.map((g) => (
          <li key={g.key} className="s-group">
            {showHeads && <div className="s-group-head">{g.label}</div>}
            <ul>
              {g.items.map((s) => {
                const mine = !s.last_from_ai && s.last_origin !== "peer";
                return (
                  <li
                    key={s.id}
                    className={`session ${p.selectedId === s.id && !p.historyChatOpen ? "on" : ""} ${s.unread ? "has-unread" : ""} ${s.ended && s.ended.state !== "keep" ? "ended" : ""} ${s.ended?.state === "keep" ? "kept" : ""}`}
                    title={rowTitle(s)}
                    tabIndex={0}
                    aria-current={p.selectedId === s.id && !p.historyChatOpen ? "true" : undefined}
                    onClick={() => p.onSelect(s.id)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        p.onSelect(s.id);
                      } else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                        e.preventDefault();
                        const r = e.currentTarget.getBoundingClientRect();
                        setMenu({ s, x: r.left + 24, y: r.bottom - 4 });
                      }
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setMenu({ s, x: e.clientX, y: e.clientY });
                    }}
                  >
                    <RowState s={s} />
                    <div className="session-body">
                      <div className="row1">
                        <span className={`s-name ${s.named ? "" : "unnamed"}`}>{s.name}</span>
                        {s.agent === "codex" && <span className="s-agent">Codex</span>}
                        <time className="s-time">{listTime(s.last_at)}</time>
                      </div>
                      <div className="row2">
                        <span className="s-preview">
                          {s.ended && <span className="s-ended">{s.ended.state === "keep" ? "이력 · " : "끝남 · "}</span>}
                          {mine && <span className="s-me">나: </span>}
                          {s.last_preview || "—"}
                        </span>
                        {s.attention > 0 && (
                          <span className="badge ask" title="응답이 필요한 결과가 있습니다">
                            <CircleHelp size={11} />
                          </span>
                        )}
                        {s.unread > 0 && (
                          <span className="badge" title={`안 읽은 결과 ${s.unread}`}>
                            {s.unread}
                          </span>
                        )}
                      </div>
                    </div>
                  </li>
                );
              })}
            </ul>
          </li>
        ))}
        {p.sessions.length === 0 && (
          <li className="empty-list">
            {p.query
              ? "검색 결과가 없습니다."
              : p.filter === "all"
                ? "아직 모은 요청이 없습니다."
                : p.filter === "history"
                  ? "이력으로 보관한 대화가 없습니다. /clear 된 대화에서 '이력으로 보관'을 고르면 여기에 모입니다."
                  : "해당하는 세션이 없습니다."}
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
      {/* 훅이 끊겼을 때만 알린다(해야 할 일) — 연결돼 있으면 조용히 */}
      {!p.hookLine.ok && (
        <button className="hook-warn" onClick={p.onSettings} title="설정 › 훅·Codex 에서 설치·업데이트">
          <span className="hook-dot" />
          <span className="hook-text">{p.hookLine.text}</span>
        </button>
      )}
      <footer className="side-foot">
        <button className="foot-btn" title={`설정 (${kbd("⌘,")})`} onClick={p.onSettings}>
          <Settings size={16} />
          <span className="btn-label">설정</span>
        </button>
        <span className="foot-sp" />
        {p.working && (
          <span className="collecting" title={p.hookLine.text}>
            <Loader2 size={12} className="spin" /> 수집 중
          </span>
        )}
        <button className="icon-btn" title={`기록 — 지난 대화 검색·보낸 메시지·이미지·세션 정리 (${kbd("⌘⇧F")})`} onClick={p.onArchive}>
          <Archive size={16} />
        </button>
        <button
          className={`icon-btn foot-count ${p.sched.held > 0 ? "warn" : ""}`}
          title={
            p.sched.held > 0
              ? `예약 ${p.sched.held}건이 전달되지 못했습니다 — 눌러서 처리하세요`
              : `예약 전송${p.sched.active ? ` — 걸려 있는 예약 ${p.sched.active}건` : ""} (입력창의 시계로 만듭니다)`
          }
          onClick={p.onSched}
        >
          <Clock size={16} />
          {(p.sched.held > 0 || p.sched.active > 0) && <span className="foot-n">{p.sched.held > 0 ? p.sched.held : p.sched.active}</span>}
        </button>
        <button
          className={`icon-btn foot-count ${p.phone && p.phone.devices > 0 ? "paired" : ""}`}
          title={
            p.phone && p.phone.devices > 0
              ? `폰(코노티) ${p.phone.devices}대 연결됨${p.phone.online > 0 ? ` · 지금 ${p.phone.online}대 보는 중` : ""} — 눌러서 관리`
              : "폰 연결 — 코노티 앱(폰)에서 이 PC 의 세션을 보고 답하기(QR)"
          }
          onClick={p.onPhone}
        >
          <Smartphone size={16} />
          {p.phone && p.phone.online > 0 && <span className="phone-live" aria-label="폰이 보는 중" />}
        </button>
        <AboutBadge toast={p.toast} />
      </footer>
    </aside>
  );
}
