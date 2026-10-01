import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Archive,
  ArchiveRestore,
  ArrowDown,
  Check,
  CheckCheck,
  CircleHelp,
  CircleSlash,
  ChevronLeft,
  Clock,
  ChevronRight,
  FileText,
  ListOrdered,
  Loader2,
  PauseCircle,
  Pin,
  PinOff,
  Reply,
  Smartphone,
  Star,
  SquareTerminal,
  Tag as TagIcon,
  X,
} from "lucide-react";
import { tryCopy } from "../clipboard";
import {
  api,
  filterActive,
  isUnread,
  NO_FILTER,
  phoneReply,
  quoteExcerpt,
  quoteLabel,
  READ_EVENT,
  splitQuote,
  type AttMeta,
  type ChatPage,
  type OutboxItem,
  type OutlineRow,
  type Quote,
  type QuotePart,
  type TagFilter,
  type QuoteTarget,
  type ReplyRow,
  type ToastFn,
  type Turn,
} from "../api";
import { clock, dayKey, dayParts, daysLeft, duration, fullTime, modelName, numeral, statusView, tokens, toolName, usd } from "../format";
import { kbd } from "../keys";
import { AttStrip, Lightbox } from "./Attachments";
import { Composer } from "./Composer";
import { Markdown } from "./Markdown";
import { Outline, outlineTitle } from "./Outline";
import { TagBar } from "./TagBar";
import { TagBadges, TagPicker } from "./TagUi";
import { useTags } from "../tags";

interface Props {
  sessionId: string;
  refreshKey: number;
  openTurnId: number | null;
  onOpenTurn: (id: number) => void;
  onRead: () => void;
  /** 이 세션(또는 모든 세션) 모두 읽음 — 되돌리기 알림은 App 이 띄운다 */
  onReadAll: (sessionId: string | null, label: string) => Promise<void>;
  toast: ToastFn;
  /** 세션이 기록에서 지워졌다(폰에서 지운 경우 등) — 대화를 닫는다 */
  onGone: () => void;
  /** 태그 관리 창을 연다 */
  onManageTags: () => void;
}

/** 요청 목록을 열어 둔 채로 두는지 — 이 PC 화면 설정일 뿐이라 브라우저 저장소에 */
const TOC_KEY = "ai-inbox.toc";
const tocSaved = () => {
  try {
    return localStorage.getItem(TOC_KEY) === "1";
  } catch {
    return false;
  }
};

const LIVE_LABEL: Record<string, string> = { busy: "작업 중", idle: "입력 대기", waiting: "승인 대기" };

const SHELL_LABEL: Record<string, string> = { powershell: "PowerShell", cmd: "명령 프롬프트(cmd)", bash: "Git Bash" };

/** Windows: 셸마다 문법이 달라 이어가기 명령을 셸별로 골라 복사한다 */
function ShellMenu({ x, y, items, onPick, onClose }: { x: number; y: number; items: { shell: string; command: string }[]; onPick: (command: string) => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  useEffect(() => {
    const el = ref.current;
    if (el) {
      const r = el.getBoundingClientRect();
      setPos({ x: Math.max(8, Math.min(x, window.innerWidth - r.width - 8)), y: Math.min(y, window.innerHeight - r.height - 8) });
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
  return (
    <div ref={ref} className="ctx-menu" role="menu" style={{ left: pos.x, top: pos.y }} onContextMenu={(e) => e.preventDefault()}>
      {items.map((it) => (
        <button
          key={it.shell}
          role="menuitem"
          title={it.command}
          onClick={() => {
            onPick(it.command);
            onClose();
          }}
        >
          <SquareTerminal size={14} /> {SHELL_LABEL[it.shell] ?? it.shell}
        </button>
      ))}
    </div>
  );
}

/** 클립보드 복사가 실패했을 때(다른 프로그램이 클립보드를 잡고 있음 등) — 명령을 직접 선택해 복사하게 보여 준다 */
function CopyFallback({ text, onClose }: { text: string; onClose: () => void }) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, [onClose]);
  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="modal copy-fallback" onClick={(e) => e.stopPropagation()}>
        <header className="modal-head">
          <h2>복사하지 못했습니다</h2>
          <button className="icon-btn" onClick={onClose} title="닫기">
            <X size={18} />
          </button>
        </header>
        <p className="set-note">다른 프로그램이 클립보드를 쓰고 있어 복사하지 못했습니다. 아래 명령을 직접 복사해 붙여 넣으세요 — 전체가 선택돼 있습니다.</p>
        <textarea ref={ref} className="copy-fallback-text" readOnly spellCheck={false} rows={3} value={text} onFocus={(e) => e.currentTarget.select()} />
      </div>
    </div>
  );
}

/** 새로 받은 요청들 중 내용이 그대로인 것은 앞의 객체를 그대로 쓴다 — 말풍선(memo)이 바뀐 것만 다시 그리게.
 *  대화는 수집기가 알릴 때마다(작업 중이면 1.5초마다) 통째로 다시 받는다. */
function keepSame(prev: Turn[], next: Turn[]): Turn[] {
  if (!prev.length) return next;
  const old = new Map(prev.map((t) => [t.id, t]));
  let same = prev.length === next.length;
  const out = next.map((t, i) => {
    const o = old.get(t.id);
    if (o && JSON.stringify(o) === JSON.stringify(t)) {
      if (prev[i] !== o) same = false;
      return o;
    }
    same = false;
    return t;
  });
  return same ? prev : out;
}

export function ChatView({ sessionId, refreshKey, openTurnId, onOpenTurn, onRead, onReadAll, toast, onGone, onManageTags }: Props) {
  const [page, setPage] = useState<ChatPage | null>(null);
  const [older, setOlder] = useState<Turn[]>([]);
  const olderRef = useRef(older);
  olderRef.current = older;
  const [hasMoreOlder, setHasMoreOlder] = useState(false);
  const [pending, setPending] = useState<ReplyRow[]>([]);
  const [outbox, setOutbox] = useState<OutboxItem[]>([]);
  /** 아직 받아 오지 않은 이전 요청 중 안 읽은 것 — 대화를 받을 때마다 잰다 */
  const [restUnread, setRestUnread] = useState(0);
  const [seed, setSeed] = useState<{ text: string; atts: AttMeta[]; n: number; prepend?: boolean } | null>(null);
  // ── 태그로 거르기 — 고른 태그가 붙은 요청만(서버가 골라 준다) ──
  const [filter, setFilter] = useState<TagFilter>(NO_FILTER);
  const filterOn = filterActive(filter);
  const filterKey = JSON.stringify(filter);
  const [filtered, setFiltered] = useState<Turn[] | null>(null);
  const filterRef = useRef(filter);
  filterRef.current = filter;
  const filteredRef = useRef(filtered);
  filteredRef.current = filtered;
  const [tagTick, setTagTick] = useState(0);
  const tagsSnap = useTags();
  const [picker, setPicker] = useState<{ id: number; anchor: DOMRect } | null>(null);
  const lastFilterKey = useRef(filterKey);
  // 메신저식 답장 — 한 세션에서 여러 작업이 돌 때 어느 요청·결과를 두고 하는 말인지
  const [quote, setQuote] = useState<QuoteTarget | null>(null);
  useEffect(() => setQuote(null), [sessionId]);
  const reply = useCallback((t: Turn, part: QuotePart) => {
    setQuote({ turn: t.id, seq: t.seq, part, text: quoteExcerpt(t, part) });
  }, []);
  const jumpToSeq = useCallback((seq: number) => {
    const t = turnsRef.current.find((x) => x.seq === seq);
    const el = t && scroller.current?.querySelector(`[data-turn="${t.id}"]`);
    if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
    else if (t && filterActive(filterRef.current)) {
      // 태그로 거른 화면에 없는 요청 — 거르기를 풀고 그 자리로
      setFilter(NO_FILTER);
      jumpTo.current = t.id;
      setJumpTick((n) => n + 1);
    }
  }, []);
  const [viewer, setViewer] = useState<{ ids: string[]; index: number } | null>(null);
  const openImages = useCallback((ids: string[], index: number) => setViewer({ ids, index }), []);
  const scroller = useRef<HTMLDivElement>(null);
  const firstLoad = useRef(true);
  const nearBottom = useRef(true);
  const readQueue = useRef(new Set<number>());
  // 부모가 새로 만든 함수로 load 가 다시 만들어지면 대화가 처음부터 다시 그려진다 — 값만 ref 로 따라간다
  const onGoneRef = useRef(onGone);
  onGoneRef.current = onGone;

  // ── 요청 목록(목차) · 하나씩 보기 ──
  const [tocOpen, setTocOpen] = useState(tocSaved);
  const [shellMenu, setShellMenu] = useState<{ x: number; y: number } | null>(null);
  const closeShellMenu = useCallback(() => setShellMenu(null), []);
  // 이어가기 명령 복사가 실패하면 명령을 직접 보여 준다(조용히 실패하면 복사된 줄 알고 붙여 넣는다)
  const [copyFail, setCopyFail] = useState<string | null>(null);
  const closeCopyFail = useCallback(() => setCopyFail(null), []);
  const copyCommand = useCallback(
    async (command: string, done: string) => {
      if (await tryCopy(command, writeText)) toast(done);
      else setCopyFail(command);
    },
    [toast],
  );
  const [outline, setOutline] = useState<OutlineRow[] | null>(null);
  /** 하나씩 보기: 이 요청의 대화만 그린다 */
  const [single, setSingle] = useState(false);
  const [focusId, setFocusId] = useState<number | null>(null);
  /** 이어 보기에서 지금 화면 위쪽에 있는 요청 */
  const [currentId, setCurrentId] = useState<number | null>(null);
  const [flashId, setFlashId] = useState<number | null>(null);
  /** 목차에서 고른 요청 — 대화에 받아 온 뒤 그 자리로 스크롤한다 */
  const jumpTo = useRef<number | null>(null);
  const [jumpTick, setJumpTick] = useState(0);
  const outlineRef = useRef<OutlineRow[] | null>(null);
  outlineRef.current = outline;
  const needOutline = tocOpen || single;
  const needOutlineRef = useRef(needOutline);
  needOutlineRef.current = needOutline;

  /** 목차는 가벼운 조회(요청 앞부분만) — 바뀐 게 없으면 그대로 둬 다시 그리지 않는다 */
  const loadOutline = useCallback(() => {
    api
      .sessionOutline(sessionId)
      .then((rows) => setOutline((prev) => (prev && JSON.stringify(prev) === JSON.stringify(rows) ? prev : rows)))
      .catch(() => setOutline([]));
  }, [sessionId]);

  const load = useCallback(async () => {
    let p: ChatPage;
    try {
      p = await api.getChat(sessionId);
    } catch (e) {
      // 세션 행이 없다 = 기록에서 지웠다(폰에서 지운 경우 포함)
      if (String(e).includes("no rows")) onGoneRef.current();
      return;
    }
    setPage((prev) => (prev && prev.session.id === p.session.id ? { ...p, turns: keepSame(prev.turns, p.turns) } : p));
    const recent = new Set(p.turns.map((t) => t.id));
    const loaded = p.turns.filter(isUnread).length + olderRef.current.filter((t) => !recent.has(t.id) && isUnread(t)).length;
    setRestUnread(Math.max(0, p.session.unread - loaded));
    api.conotiPending(sessionId).then(setPending).catch(() => setPending([]));
    api.sessionOutbox(sessionId).then(setOutbox).catch(() => setOutbox([]));
    setHasMoreOlder((prev) => (firstLoad.current ? p.has_more : prev));
    if (needOutlineRef.current) loadOutline();
  }, [sessionId, loadOutline]);

  useEffect(() => {
    firstLoad.current = true;
    setPage(null);
    setOlder([]);
    setOutline(null);
    setSingle(false);
    setFocusId(null);
    setCurrentId(null);
    setFilter(NO_FILTER);
    setFiltered(null);
    setPicker(null);
    load();
  }, [sessionId, load]);

  // 태그로 거른 요청 — 대화가 바뀌거나 태그가 바뀔 때마다 다시 받는다(서버가 고른다: 안 받아 둔 이전 요청 포함, 최대 500)
  useEffect(() => {
    if (!filterOn) {
      setFiltered(null);
      return;
    }
    let dead = false;
    api
      .getChatTagged(sessionId, filterRef.current, undefined, 500)
      .then((p) => {
        if (dead) return;
        setFiltered((prev) => keepSame(prev ?? [], p.turns));
      })
      .catch(() => {});
    return () => {
      dead = true;
    };
  }, [sessionId, filterOn, filterKey, refreshKey, tagsSnap.ov, tagTick]);
  // 거르기를 바꾸거나 풀면 가장 최근 요청으로
  useEffect(() => {
    if (lastFilterKey.current === filterKey) return;
    lastFilterKey.current = filterKey;
    toBottom();
  }, [filterKey, filtered]); // eslint-disable-line react-hooks/exhaustive-deps

  /** 태그를 고친 뒤 — 받아 둔 요청(최근·이전)의 태그와 거른 목록을 새로 */
  const refreshTags = useCallback(async () => {
    await load();
    const old = olderRef.current;
    if (old.length) {
      try {
        const p = await api.getChat(sessionId, old[old.length - 1].seq + 1, old.length);
        setOlder(p.turns);
      } catch {
        /* 다음 갱신에서 */
      }
    }
    setTagTick((n) => n + 1);
  }, [load, sessionId]);

  useEffect(() => {
    if (needOutline) loadOutline();
  }, [needOutline, loadOutline]);

  useEffect(() => {
    try {
      localStorage.setItem(TOC_KEY, tocOpen ? "1" : "0");
    } catch {
      /* 저장 못 해도 이번 창에서는 그대로 */
    }
  }, [tocOpen]);

  useEffect(() => {
    if (!firstLoad.current) load();
  }, [refreshKey, load]);

  const turns = useMemo(() => {
    const recent = page?.turns ?? [];
    const seen = new Set(recent.map((t) => t.id));
    return [...older.filter((t) => !seen.has(t.id)), ...recent];
  }, [page, older]);
  const turnsRef = useRef<Turn[]>([]);
  turnsRef.current = turns;


  /** 받아 둔 요청들의 읽음을 한꺼번에 바꾼다 — at 이 null 이면 안 읽음으로 */
  const applyRead = useCallback((ids: number[], at: string | null) => {
    if (!ids.length) return;
    const set = new Set(ids);
    const mark = (list: Turn[]) => {
      let hit = false;
      const out = list.map((t) => {
        if (!set.has(t.id) || (at ? t.read_at : !t.read_at)) return t;
        hit = true;
        return { ...t, read_at: at };
      });
      return hit ? out : list;
    };
    setPage((p) => (p ? { ...p, turns: mark(p.turns) } : p));
    setOlder((o) => mark(o));
    setFiltered((f) => (f ? mark(f) : f));
    setOutline((rows) => {
      if (!rows) return rows;
      let hit = false;
      const out = rows.map((r) => {
        if (!set.has(r.id) || r.unread === !at) return r;
        hit = true;
        return { ...r, unread: !at };
      });
      return hit ? out : rows;
    });
  }, []);

  // 다른 곳(목록의 모두 읽음 · 되돌리기)에서 바뀐 읽음을 받아 둔 요청에 반영
  useEffect(() => {
    const on = (e: Event) => {
      const d = (e as CustomEvent<{ ids: number[]; at: string | null }>).detail;
      applyRead(d.ids, d.at);
    };
    window.addEventListener(READ_EVENT, on);
    return () => window.removeEventListener(READ_EVENT, on);
  }, [applyRead]);

  const firstUnread = turns.find(isUnread);
  const unreadCount = turns.filter(isUnread).length;
  const turnById = useMemo(() => new Map(turns.map((t) => [t.id, t])), [turns]);
  /** 목차의 안 읽음 — 대화에 받아 둔 요청이면 그쪽 상태(읽는 즉시 바뀐다)를 따른다 */
  const unreadOf = useCallback(
    (r: OutlineRow) => {
      const t = turnById.get(r.id);
      return t ? isUnread(t) : r.unread;
    },
    [turnById],
  );
  const outlineUnread = outline ? outline.filter(unreadOf).length : 0;
  /** 이 세션 전체(아직 안 받은 이전 요청 포함)의 안 읽음 */
  const sessionUnread = outline ? outlineUnread : unreadCount + restUnread;

  const toBottom = () => {
    nearBottom.current = true;
    requestAnimationFrame(() => {
      const el = scroller.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  };

  const readAllHere = async () => {
    try {
      await onReadAll(sessionId, page?.session.name ?? "이 세션");
      // 모두 읽음 → 가장 최근 대화로 (되돌리기는 스크롤을 건드리지 않는다)
      toBottom();
    } catch (e) {
      toast(String(e));
    }
  };

  /** 목차의 요청이 대화에 없으면(이전 요청) 그 요청까지 받아 온다 */
  const ensureLoaded = useCallback(
    async (id: number) => {
      if (turnsRef.current.some((t) => t.id === id)) return true;
      const rows = outlineRef.current;
      const target = rows?.find((r) => r.id === id);
      if (!rows || !target) return false;
      let first = turnsRef.current[0];
      const got: Turn[] = [];
      for (let guard = 0; first && target.seq < first.seq && guard < 20; guard++) {
        const before = first.seq;
        const need = rows.filter((r) => r.seq >= target.seq && r.seq < before).length;
        const p = await api.getChat(sessionId, before, Math.min(500, need + 2));
        if (!p.turns.length) break;
        got.unshift(...p.turns);
        setHasMoreOlder(p.has_more);
        first = p.turns[0];
      }
      if (got.length) setOlder((o) => [...got, ...o]);
      return got.some((t) => t.id === id);
    },
    [sessionId],
  );

  const pick = useCallback(
    async (id: number) => {
      // 태그로 거른 화면에 없는 요청을 목차에서 골랐다 — 거르기를 풀고 그 자리로
      if (filterActive(filterRef.current) && !filteredRef.current?.some((t) => t.id === id)) setFilter(NO_FILTER);
      await ensureLoaded(id);
      if (single) {
        setFocusId(id);
      } else {
        jumpTo.current = id;
        setJumpTick((n) => n + 1);
      }
      setCurrentId(id);
    },
    [ensureLoaded, single],
  );

  const setMode = useCallback(
    async (on: boolean) => {
      if (on) {
        const rows = outlineRef.current;
        const list = turnsRef.current;
        const id = currentId ?? rows?.[rows.length - 1]?.id ?? list[list.length - 1]?.id ?? null;
        if (id != null) await ensureLoaded(id);
        setFocusId(id);
        setSingle(true);
      } else {
        setSingle(false);
        if (focusId != null) {
          jumpTo.current = focusId;
          setJumpTick((n) => n + 1);
        }
      }
    },
    [currentId, focusId, ensureLoaded],
  );

  const closeToc = useCallback(() => {
    setTocOpen(false);
    if (single) setMode(false);
  }, [single, setMode]);

  // 하나씩 보기의 앞뒤 요청 — 목차 순서(아직 없으면 받아 둔 대화 순서)
  /** 화면에 그리는 요청 — 태그로 거르면 서버가 골라 준 것, 아니면 받아 둔 전부 */
  const visible = filterOn ? (filtered ?? []) : turns;
  const order: { id: number }[] = filterOn ? visible : (outline ?? turns);
  const focusIndex = focusId != null ? order.findIndex((r) => r.id === focusId) : -1;
  const step = (d: number) => {
    const next = order[focusIndex + d];
    if (next) pick(next.id);
  };
  const shownTurns = useMemo(() => (single && focusId != null ? visible.filter((t) => t.id === focusId) : visible), [single, focusId, visible]);

  // 처음 열 때: 안 읽은 첫 결과로, 없으면 맨 아래로. 이후 갱신: 바닥 근처면 따라 내려간다.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !page) return;
    if (firstLoad.current) {
      firstLoad.current = false;
      const target = firstUnread ? el.querySelector<HTMLElement>(`[data-turn="${firstUnread.id}"]`) : null;
      if (target) {
        const user = target.previousElementSibling as HTMLElement | null;
        el.scrollTop = (user ?? target).offsetTop - 64;
      } else {
        el.scrollTop = el.scrollHeight;
      }
      return;
    }
    if (nearBottom.current && !single) el.scrollTop = el.scrollHeight;
  }, [page, outbox, filtered]); // eslint-disable-line react-hooks/exhaustive-deps

  // 목차에서 고른 요청으로 — 요청 말풍선이 위에 오게, 잠깐 테두리로 짚어 준다
  useLayoutEffect(() => {
    const el = scroller.current;
    const id = jumpTo.current;
    if (!el || id == null) return;
    const target = el.querySelector<HTMLElement>(`[data-pair="${id}"]`);
    if (!target) return;
    jumpTo.current = null;
    el.scrollTop = Math.max(0, target.offsetTop - 16);
    nearBottom.current = false;
    setFlashId(id);
  }, [jumpTick, turns, single]);
  useEffect(() => {
    if (flashId == null) return;
    const t = window.setTimeout(() => setFlashId(null), 1600);
    return () => window.clearTimeout(t);
  }, [flashId]);

  // 하나씩 보기에서 요청을 바꾸면 맨 위부터
  useLayoutEffect(() => {
    if (single && scroller.current) scroller.current.scrollTop = 0;
  }, [single, focusId]);

  // ⌘⇧O 요청 목록 열고 닫기 · 하나씩 보기에서 Alt+←/→ 앞뒤 요청
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "o") {
        e.preventDefault();
        if (tocOpen) closeToc();
        else setTocOpen(true);
      } else if (single && e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        e.preventDefault();
        step(e.key === "ArrowLeft" ? -1 : 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // 보이는 결과를 읽음 처리 — 창이 앞에 있고 말풍선이 60% 이상 1초 가까이 보였을 때
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const timers = new Map<Element, number>();
    const flush = () => {
      const ids = [...readQueue.current];
      if (!ids.length) return;
      readQueue.current.clear();
      api.markRead(ids).then(() => {
        applyRead(ids, new Date().toISOString());
        onRead();
      });
    };
    const io = new IntersectionObserver(
      (entries) => {
        for (const en of entries) {
          const id = Number((en.target as HTMLElement).dataset.turn);
          if (en.isIntersecting && en.intersectionRatio >= 0.6) {
            if (!timers.has(en.target)) {
              timers.set(
                en.target,
                window.setTimeout(() => {
                  timers.delete(en.target);
                  if (document.hasFocus()) {
                    readQueue.current.add(id);
                    flush();
                  }
                }, 900),
              );
            }
          } else if (timers.has(en.target)) {
            clearTimeout(timers.get(en.target));
            timers.delete(en.target);
          }
        }
      },
      { root: el, threshold: [0, 0.6, 1] },
    );
    el.querySelectorAll<HTMLElement>(".ai[data-unread='1']").forEach((n) => io.observe(n));
    return () => {
      io.disconnect();
      timers.forEach((t) => clearTimeout(t));
    };
  }, [shownTurns, onRead, applyRead]);

  /** 화면 위쪽에 걸친 요청 — 요청 쌍은 위에서 아래로 쌓이므로 이진 탐색 */
  const findCurrent = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const pairs = el.querySelectorAll<HTMLElement>(".pair[data-pair]");
    const y = el.scrollTop + 24;
    let lo = 0;
    let hi = pairs.length - 1;
    let hit = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const p = pairs[mid];
      if (p.offsetTop + p.offsetHeight <= y) lo = mid + 1;
      else {
        hit = mid;
        hi = mid - 1;
      }
    }
    const id = hit >= 0 ? Number(pairs[hit].dataset.pair) : null;
    setCurrentId((cur) => (cur === id ? cur : id));
  }, []);
  const curFrame = useRef(0);
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    nearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    // 목록을 열어 둔 때만, 한 프레임에 한 번
    if (!tocOpen || single || curFrame.current) return;
    curFrame.current = requestAnimationFrame(() => {
      curFrame.current = 0;
      findCurrent();
    });
  };
  useEffect(() => () => cancelAnimationFrame(curFrame.current), []);
  // 목록을 열 때 지금 보고 있는 요청을 짚는다(스크롤 전에도)
  useEffect(() => {
    if (tocOpen && !single && page) findCurrent();
  }, [tocOpen, single, page != null, findCurrent]); // eslint-disable-line react-hooks/exhaustive-deps

  const loadOlder = async () => {
    const first = turns[0];
    if (!first) return;
    const el = scroller.current;
    const before = el ? el.scrollHeight - el.scrollTop : 0;
    const p = await api.getChat(sessionId, first.seq, 60);
    setOlder((o) => [...p.turns, ...o]);
    setHasMoreOlder(p.has_more);
    requestAnimationFrame(() => {
      if (el) el.scrollTop = el.scrollHeight - before;
    });
  };

  const jumpUnread = () => {
    const el = scroller.current;
    const next = visible.find(isUnread);
    if (!el || !next) return;
    if (single) {
      setFocusId(next.id);
      setCurrentId(next.id);
      return;
    }
    const target = el.querySelector<HTMLElement>(`[data-turn="${next.id}"]`);
    target?.scrollIntoView({ behavior: "smooth", block: "center" });
  };

  if (!page) return <section className="chat loading" />;
  const s = page.session;

  let lastDay = "";
  return (
    <section className={`chat ${s.ended ? `ended st-${s.ended.state}` : ""}`}>
      <header className="chat-head" data-tauri-drag-region>
        <div className="chat-title">
          <h1 className={s.named ? "" : "unnamed"}>{s.name}</h1>
          <div className="chat-sub">
            {s.project_dir && <span title={s.project_dir}>{s.project_dir}</span>}
            {s.git_branch && s.git_branch !== "HEAD" && <span>{s.git_branch}</span>}
            {s.agent === "codex" && (
              <span className="agent-tag" title="OpenAI Codex 세션 — 기록은 ~/.codex/sessions">
                Codex{s.cc_version ? ` ${s.cc_version}` : ""}
              </span>
            )}
            <span className={`live ${s.live_status ?? "off"}`}>{s.live_status ? LIVE_LABEL[s.live_status] ?? s.live_status : "세션 종료"}</span>
            {s.model && <span>{modelName(s.model)}</span>}
            {s.cost_usd != null && <span title="Claude Code 가 기록한 세션 누적 비용(API 환산)">{usd(s.cost_usd)}</span>}
          </div>
        </div>
        <div className="chat-actions">
          <label
            className={`phone-mode ${s.conoti_mode === 0 ? "on" : ""}`}
            title="연결된 폰(코노티 앱)에서 이 세션에 답을 보내 작업을 이어갈 수 있게 할지 — 폰 연결은 설정에서"
          >
            <Smartphone size={15} />
            <select
              value={s.conoti_mode}
              onChange={async (e) => {
                await api.conotiSetSessionMode(sessionId, Number(e.target.value));
                await load();
                toast(Number(e.target.value) === 0 ? "이 세션은 폰 답을 받습니다" : "이 세션은 폰 답을 받지 않습니다");
              }}
            >
              <option value={0}>폰 답 받기</option>
              <option value={1}>폰 답 막기</option>
            </select>
          </label>
          {s.send_mode === "terminal" && (
            <span className="chan-warn" title="실행 중인 세션에 앱·폰에서 말을 넣으려면 설정에서 훅을 다시 설치하세요">
              훅 업데이트 필요
            </span>
          )}
          <button
            className="text-btn"
            title={s.attach_command ?? (s.resume_shells?.length ? "쓰는 셸을 골라 이어가기 명령을 복사" : s.resume_command)}
            onClick={async (e) => {
              // Windows: 셸마다 문법이 달라 고르게 한다(백그라운드 세션을 여는 명령은 어느 셸에서나 같다)
              if (!s.attach_command && s.resume_shells?.length) {
                const r = e.currentTarget.getBoundingClientRect();
                setShellMenu({ x: r.left, y: r.bottom + 4 });
                return;
              }
              await copyCommand(s.attach_command ?? s.resume_command, s.attach_command ? "백그라운드 세션을 여는 명령을 복사했습니다" : "이어가기 명령을 복사했습니다");
            }}
          >
            <SquareTerminal size={16} /> {s.attach_command ? "터미널에서 열기" : "이어가기"}
          </button>
          {shellMenu && s.resume_shells && (
            <ShellMenu
              x={shellMenu.x}
              y={shellMenu.y}
              items={s.resume_shells}
              onClose={closeShellMenu}
              onPick={(command) => copyCommand(command, "이어가기 명령을 복사했습니다")}
            />
          )}
          {copyFail !== null && <CopyFallback text={copyFail} onClose={closeCopyFail} />}
          <button
            className={`toc-btn ${tocOpen ? "on" : ""}`}
            title={`이 세션의 요청 목록 — 골라서 그 자리로 가거나 하나만 보기 (${kbd("⌘⇧O")})`}
            aria-pressed={tocOpen}
            onClick={() => (tocOpen ? closeToc() : setTocOpen(true))}
          >
            <ListOrdered size={16} />
            <span>요청 {s.turns}</span>
          </button>
          <button className="icon-btn" title="이 세션 모두 읽음" disabled={!sessionUnread} onClick={readAllHere}>
            <CheckCheck size={17} />
          </button>
          <button
            className="icon-btn"
            title={s.pinned ? "고정 해제" : "목록 위에 고정"}
            onClick={async () => {
              await api.setPinned(sessionId, !s.pinned);
              await load();
              onRead();
            }}
          >
            {s.pinned ? <PinOff size={17} /> : <Pin size={17} />}
          </button>
          <button
            className="icon-btn"
            title={s.archived ? "목록으로 되돌리기" : "보관 — 목록과 폰에서 빼고 기록 검색에는 남긴다"}
            onClick={async () => {
              await api.setHidden(sessionId, !s.archived);
              await load();
              onRead();
              toast(s.archived ? "목록으로 되돌렸습니다" : `보관했습니다 — 기록(${kbd("⌘⇧F")}) › 세션 › 보관함에서 되돌릴 수 있습니다`);
            }}
          >
            {s.archived ? <ArchiveRestore size={17} /> : <Archive size={17} />}
          </button>
        </div>
      </header>

      {s.archived && (
        <div className="archived-bar">
          <Archive size={14} />
          <span>보관한 세션입니다. 목록과 폰에는 보이지 않고, 새 요청이나 결과가 오면(여기서 보내도) 저절로 목록으로 돌아옵니다.</span>
          <button
            className="more"
            onClick={async () => {
              await api.setHidden(sessionId, false);
              await load();
              onRead();
            }}
          >
            목록으로 되돌리기
          </button>
        </div>
      )}

      {s.ended && (
        <div className={`ended-bar st-${s.ended.state}`}>
          <Clock size={14} />
          <span>
            {s.ended.state === "keep" ? (
              <>/clear 로 끝난 대화입니다. <strong>이력으로 보관 중</strong> — 자동으로 지워지지 않고 이력 탭·이력 찾기에서 볼 수 있습니다.</>
            ) : s.ended.state === "ask" ? (
              <>/clear 로 끝난 대화입니다. 아직 처리를 정하지 않았습니다 — <strong>자동으로 지워지지 않습니다.</strong></>
            ) : (
              <>
                /clear 로 끝난 대화입니다. <strong>{daysLeft(s.ended.purge_at) ?? "?"}일 뒤({fullTime(s.ended.purge_at).slice(0, 10)})</strong> 이 앱의 사본이 삭제됩니다 — Claude Code 에서{" "}
                <code>/resume</code>·<code>/rewind</code> 로 되돌릴 수 있는 기간이 끝난 뒤입니다. 이어서 쓰면 취소됩니다.
              </>
            )}
          </span>
          {s.ended.state !== "keep" && (
            <button
              className="more"
              onClick={async () => {
                await api.clearDecide([sessionId], "keep");
                await load();
                onRead();
                toast("이력으로 보관합니다 — 이력 탭에서 볼 수 있습니다");
              }}
            >
              이력으로 보관
            </button>
          )}
          {s.ended.state !== "purge" && (
            <button
              className="more"
              onClick={async () => {
                await api.clearDecide([sessionId], "purge");
                await load();
                onRead();
                toast("삭제 예약했습니다");
              }}
            >
              삭제 예약
            </button>
          )}
          {s.ended.state === "purge" && !s.ended.asked && (
            <button
              className="more"
              onClick={async () => {
                await api.clearAck([sessionId]);
                await load();
                onRead();
              }}
            >
              그대로 두기
            </button>
          )}
        </div>
      )}

      {pending.length > 0 && (
        <div className="confirm-bar">
          {pending.map((r) => (
            <div key={r.reply_id} className="confirm-item">
              <strong>폰 답</strong>
              <span className="c-text">{r.text || (r.atts?.length ? "(이미지만 보냄)" : "")}</span>
              {r.atts?.length > 0 && <AttStrip ids={r.atts} px={36} onOpen={openImages} />}
              <button
                className="btn primary"
                onClick={async () => {
                  await api.conotiDecide(r.reply_id, true);
                  toast("세션에 전달합니다");
                  load();
                }}
              >
                전달
              </button>
              <button
                className="btn"
                onClick={async () => {
                  await api.conotiDecide(r.reply_id, false);
                  toast("거절했습니다");
                  load();
                }}
              >
                거절
              </button>
            </div>
          ))}
        </div>
      )}

      <TagBar
        sessionId={sessionId}
        refreshKey={refreshKey + tagTick}
        filter={filter}
        onFilter={setFilter}
        onManage={onManageTags}
        onContext={(text) => setSeed({ text, atts: [], n: Date.now(), prepend: true })}
        toast={toast}
      />

      <div className={`chat-body ${tocOpen ? "with-toc" : ""}`}>
        <div className="chat-scroll" ref={scroller} onScroll={onScroll}>
          {single && (
            <div className="focus-bar">
              <button className="icon-btn" title="앞 요청 (Alt+←)" disabled={focusIndex <= 0} onClick={() => step(-1)}>
                <ChevronLeft size={16} />
              </button>
              <span className="focus-pos">
                요청 <strong>{focusIndex + 1}</strong> / {order.length}
              </span>
              <button
                className="icon-btn"
                title="다음 요청 (Alt+→)"
                disabled={focusIndex < 0 || focusIndex >= order.length - 1}
                onClick={() => step(1)}
              >
                <ChevronRight size={16} />
              </button>
              <span className="focus-title">{focusIndex >= 0 && outline ? outlineTitle(outline[focusIndex]) : ""}</span>
              <button className="more" onClick={() => setMode(false)}>
                대화 이어 보기
              </button>
            </div>
          )}
          <div className="chat-inner">
            {hasMoreOlder && !single && !filterOn && (
              <button className="load-older" onClick={loadOlder}>
                이전 요청 더 보기
              </button>
            )}
            {shownTurns.map((t) => {
              const key = dayKey(t.prompt_at);
              const showDay = key !== lastDay;
              lastDay = key;
              const dp = dayParts(t.prompt_at);
              return (
                <div key={t.id} className={`pair ${flashId === t.id ? "flash" : ""}`} data-pair={t.id}>
                  {showDay && (
                    <div className="day">
                      <span className="day-n">{dp.day}</span>
                      <span className="day-rest">{dp.rest}</span>
                    </div>
                  )}
                  {firstUnread?.id === t.id && !single && (
                    <div className="unread-rule">
                      <span>안 읽은 결과 {unreadCount}</span>
                    </div>
                  )}
                  <UserBubble t={t} onOpenImages={openImages} onReply={reply} onJump={jumpToSeq} onTags={(anchor) => setPicker({ id: t.id, anchor })} onTagsChanged={refreshTags} />
                  <AiBubble t={t} open={openTurnId === t.id} onOpenTurn={onOpenTurn} onReply={reply} />
                </div>
              );
            })}
            {filterOn && filtered && filtered.length === 0 && <p className="tag-empty">고른 태그가 붙은 요청이 없습니다.</p>}
            {!single &&
              !filterOn &&
              outbox.map((o) => (
              <OutboxBubble
                key={o.rid}
                o={o}
                onOpenImages={openImages}
                onCancel={async () => {
                  await api.cancelMessage(o.rid);
                  load();
                }}
                onRewrite={async () => {
                  setSeed({ text: o.text ?? "", atts: o.atts ?? [], n: Date.now() });
                  await api.cancelMessage(o.rid);
                  load();
                }}
              />
            ))}
          </div>
        </div>

        {sessionUnread > 0 && (
          <div className="jump-group">
            <button className="jump-read" onClick={readAllHere} title="이 세션의 안 읽은 결과를 한 번에 읽음으로 — 되돌릴 수 있습니다">
              <CheckCheck size={15} />
              모두 읽음
            </button>
            {unreadCount > 0 && (
              <button className="jump" onClick={jumpUnread} title="다음 안 읽은 결과로">
                <ArrowDown size={16} />
                <span>{unreadCount}</span>
              </button>
            )}
          </div>
        )}
        {tocOpen && (
          <Outline
            rows={outline}
            currentId={single ? focusId : currentId}
            unreadOf={unreadOf}
            tagFilter={filterOn ? filter : null}
            single={single}
            onSingle={setMode}
            onPick={pick}
            onClose={closeToc}
          />
        )}
      </div>

      <Composer
        session={s}
        seed={seed}
        toast={toast}
        quote={quote}
        onClearQuote={() => setQuote(null)}
        onJump={jumpToSeq}
        onSent={() => {
          nearBottom.current = true;
          load();
        }}
      />
      {picker && (
        <TagPicker
          turnId={picker.id}
          sessionId={sessionId}
          current={(visible.find((t) => t.id === picker.id) ?? turnById.get(picker.id))?.tags ?? []}
          anchor={picker.anchor}
          onClose={() => setPicker(null)}
          onChanged={refreshTags}
          toast={toast}
        />
      )}
      {viewer && <Lightbox ids={viewer.ids} index={viewer.index} onClose={() => setViewer(null)} toast={toast} />}
    </section>
  );
}

/** 보냈지만 아직 대화에 요청으로 잡히지 않은 말 */
function OutboxBubble({
  o,
  onCancel,
  onRewrite,
  onOpenImages,
}: {
  o: OutboxItem;
  onCancel: () => void;
  onRewrite: () => void;
  onOpenImages: (ids: string[], index: number) => void;
}) {
  const state =
    o.state === "confirm"
      ? "PC 확인 대기 — 위에서 전달하거나 거절하세요"
      : o.state === "delivering"
        ? (o.note ?? "보내는 중…")
        : o.state === "delivered"
          ? `전달됨${o.note ? ` · ${o.note}` : ""}`
          : `보내지 못함${o.note ? ` — ${o.note}` : ""}`;
  return (
    <div className="user-row">
      <div className={`user pending s-${o.state}`}>
        <div className="who">
          {o.from_phone ? (
            <span className="via-phone">{o.sched ? "나 · 폰에서 예약" : "나 · 폰에서"}</span>
          ) : o.sched ? (
            "나 · AI Inbox 예약"
          ) : (
            "나 · AI Inbox"
          )}
          <time>{clock(o.at)}</time>
        </div>
        {o.quote && (
          <div className="quote-block">
            <span className="q-label">{quoteLabel(o.quote)}에 답장</span>
          </div>
        )}
        {o.text && <div className="user-text">{o.text}</div>}
        <AttStrip ids={(o.atts ?? []).map((a) => a.id)} onOpen={onOpenImages} />
        <div className="send-state">
          {(o.state === "delivering" || o.state === "delivered") && <Loader2 size={12} className="spin" />}
          <span>{state}</span>
          {/* 예약이 넣은 줄은 폰이 건 것이어도 PC 에서 거둘 수 있다(conoti::cancel_desktop) */}
          {(!o.from_phone || o.sched) && o.state === "delivering" && (
            <button className="more" onClick={onCancel}>
              취소
            </button>
          )}
          {(!o.from_phone || o.sched) && o.state === "rejected" && (
            <>
              <button className="more" onClick={onRewrite}>
                다시 쓰기
              </button>
              <button className="more" onClick={onCancel}>
                지우기
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

const UserBubble = memo(function UserBubble({
  t,
  onOpenImages,
  onReply,
  onJump,
  onTags,
  onTagsChanged,
}: {
  t: Turn;
  onOpenImages: (ids: string[], index: number) => void;
  onReply: (t: Turn, part: QuotePart) => void;
  onJump: (seq: number) => void;
  onTags: (anchor: DOMRect) => void;
  onTagsChanged: () => void;
}) {
  const [more, setMore] = useState(false);
  const phone = phoneReply(t.prompt_text);
  const { quote, rest } = splitQuote((phone ? phone.body : t.prompt_text) ?? "");
  const text = rest.trim();
  const long = text.length > 600 || text.split("\n").length > 12;
  return (
    <div className="user-row">
      <div className="user">
        <div className="who">
          {phone ? (
            <span className="via-phone">나 · 폰에서{phone.title ? ` — ${phone.title}` : ""}</span>
          ) : t.origin === "peer" ? (
            `${t.peer_name ?? "다른 세션"} 이 보냄`
          ) : t.origin === "inbox" ? (
            "나 · AI Inbox"
          ) : t.origin === "sched" ? (
            "나 · AI Inbox 예약"
          ) : t.origin === "channel" ? (
            "채널에서 옴"
          ) : (
            "나"
          )}
          {t.prompt_source === "queued" && " · 대기열"}
          {t.prompt_source === "mid-turn" && " · 작업 중에 보냄"}
          <time>{clock(t.prompt_at)}</time>
          <button className="reply-btn" title="이 요청에 답장" onClick={() => onReply(t, "prompt")}>
            <Reply size={13} />
          </button>
          <button className="reply-btn" title="이 요청의 태그 고치기" onClick={(e) => onTags(e.currentTarget.getBoundingClientRect())}>
            <TagIcon size={13} />
          </button>
        </div>
        {quote && <QuoteBlock q={quote} onJump={onJump} />}
        {t.slash_command && <code className="slash">{t.slash_command}</code>}
        {text && <div className={`user-text ${long && !more ? "clamp" : ""}`}>{text}</div>}
        {long && (
          <button className="more" onClick={() => setMore(!more)}>
            {more ? "접기" : "더 보기"}
          </button>
        )}
        <AttStrip ids={t.atts ?? []} onOpen={onOpenImages} />
        {t.tags?.length > 0 && <TagBadges tags={t.tags} turnId={t.id} onChanged={onTagsChanged} />}
      </div>
    </div>
  );
});

/** 본문이 칸을 넘칠 때만 아래를 흐리게 */
function Clamp({ children }: { children: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [over, setOver] = useState(false);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => setOver(el.scrollHeight > el.clientHeight + 2);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [children]);
  return (
    <div ref={ref} className={`ai-body ${over ? "over" : ""}`}>
      {children}
    </div>
  );
}

function StatusIcon({ t }: { t: Turn }) {
  if (t.status === "running" || t.status === "background") return <Loader2 size={15} className="spin" />;
  if (t.status === "waiting") return <PauseCircle size={15} />;
  if (t.status === "interrupted" || t.status === "stopped") return <CircleSlash size={15} />;
  if (t.needs_input) return <CircleHelp size={15} />;
  return <Check size={15} />;
}

/** 요청 뒤 지난 시간 — 이 숫자만 1초마다 다시 그린다(예전엔 대화 전체가 1초마다 다시 그려졌다) */
function Elapsed({ since }: { since: string }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  return <>{numeral(now - new Date(since).getTime())}</>;
}

/** 보낸 말 위의 인용 — 누르면 그 요청으로 */
function QuoteBlock({ q, onJump }: { q: Quote; onJump: (seq: number) => void }) {
  return (
    <button className="quote-block" title="답장한 요청으로 가기" onClick={() => onJump(q.seq)}>
      <span className="q-label">{quoteLabel(q)}</span>
      <span className="q-text">{q.text}</span>
    </button>
  );
}

const AiBubble = memo(function AiBubble({
  t,
  open,
  onOpenTurn,
  onReply,
}: {
  t: Turn;
  open: boolean;
  onOpenTurn: (id: number) => void;
  onReply: (t: Turn, part: QuotePart) => void;
}) {
  const st = statusView(t.status, t.needs_input, t.pending_bg);
  const live = t.status === "running" || t.status === "background" || t.status === "waiting";
  const unread = isUnread(t);
  const onOpen = () => onOpenTurn(t.id);
  const body = t.response_text?.trim() || (live ? t.understanding?.trim() : "") || "";
  const step = t.last_step;
  const stepLine =
    live && step
      ? step[0] === "tool"
        ? `${toolName(step[1])} — ${step[2] ?? ""}`
        : step[0] === "task"
          ? `백그라운드 알림 — ${step[2] ?? ""}`
          : (step[2] ?? "").split("\n")[0]
      : null;

  return (
    <div
      className={`ai tone-${st.tone} ${unread ? "unread" : ""} ${open ? "open" : ""}`}
      data-turn={t.id}
      data-unread={unread ? "1" : "0"}
      onClick={(e) => {
        const tag = (e.target as HTMLElement).closest("a,button");
        if (!tag) onOpen();
      }}
    >
      <div className="ai-head">
        <span className="ai-status">
          <StatusIcon t={t} />
          {st.label}
          {unread && <span className="new">새 결과</span>}
          {t.starred && <Star size={13} className="star" />}
        </span>
        <span className="ai-num" title={live ? "요청 후 지난 시간" : `걸린 시간 ${duration(t.duration_ms)}`}>
          {live ? <Elapsed since={t.prompt_at} /> : numeral(t.duration_ms)}
        </span>
      </div>

      {stepLine && <div className="ai-step">지금: {stepLine}</div>}
      {t.summary && <p className="ai-summary">{t.summary}</p>}
      {body ? (
        <Clamp>
          <Markdown>{body.length > 6000 ? body.slice(0, 6000) + "\n\n…" : body}</Markdown>
        </Clamp>
      ) : (
        !live && (
          <div className="ai-empty">
            {t.prompt_source === "mid-turn"
              ? "따로 답한 글 없이 앞 요청 안에서 이어졌습니다."
              : t.api_calls === 0 && t.status !== "interrupted" && (t.origin === "inbox" || t.origin === "sched" || phoneReply(t.prompt_text) !== null)
                ? "답을 받지 못했습니다 — 세션이 응답 없이 끝났습니다. 다시 보내 보세요."
                : "응답 텍스트 없이 끝났습니다."}
          </div>
        )
      )}

      <div className="ai-foot">
        <span className="facts">
          {t.tool_calls > 0 && <span>도구 {t.tool_calls}</span>}
          {t.files_changed > 0 && <span>파일 {t.files_changed}</span>}
          {t.subagent_count > 0 && <span>에이전트 {t.subagent_count}</span>}
          {t.output_tokens > 0 && <span>출력 {tokens(t.output_tokens)}</span>}
          {t.model && <span>{modelName(t.model)}</span>}
          {t.ended_at && <time>{clock(t.ended_at)}</time>}
        </span>
        <button className="reply-btn" title="이 결과에 답장" onClick={() => onReply(t, "response")}>
          <Reply size={13} /> 답장
        </button>
        <button className="open-doc" onClick={onOpen}>
          <FileText size={14} /> 문서로 보기
        </button>
      </div>
    </div>
  );
});
