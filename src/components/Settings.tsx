import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { Bell, Clock, Copy, Eraser, Info, MessageSquareText, Plug, Search, Smartphone, Tag, X, type LucideIcon } from "lucide-react";
import { api, type AppInfo, type CodexStatus, type HookStatus, type RelayKey, type RelayOffer, type RelayStatus, type UpdateState } from "../api";
import { fullTime } from "../format";
import { kbd } from "../keys";
import { SETTINGS_BLOCKS, SETTINGS_CATS, SETTINGS_ENTRIES, type BlockId, type SettingsCat, type SettingsEntry } from "../settingsIndex";
import { flatHits, searchSettings, type Segment } from "../settingsSearch";
import { CopyReport, useAbout } from "./About";
import { ClearSection, HistorySection, ScheduleSection } from "./ClearSettings";
import { Note } from "./Note";

interface Props {
  onClose: () => void;
  onHooksChanged: (s: HookStatus) => void;
  /** 요청 태그 관리 창을 연다 */
  onTags: () => void;
  toast: (msg: string) => void;
}

const NOTIFY_MIN = [0, 30, 60, 120, 300];
const BACKFILL = [1, 3, 7, 14, 30];

const CAT_KEY = "ai-inbox.settings-cat";
const CAT_ICON: Record<SettingsCat, LucideIcon> = {
  general: Bell,
  agents: Plug,
  history: Eraser,
  tags: Tag,
  schedule: Clock,
  ai: MessageSquareText,
  phone: Smartphone,
  about: Info,
};
const CAT_ORDER = SETTINGS_CATS.map((c) => c.id);
const CAT_LABELS = Object.fromEntries(SETTINGS_CATS.map((c) => [c.id, c.label])) as Record<SettingsCat, string>;
/** 화면 표기 — 단축키는 OS 에 맞게(⌘ → Ctrl+) */
const ENTRIES: SettingsEntry[] = SETTINGS_ENTRIES.map((e) => ({ ...e, title: kbd(e.title), desc: kbd(e.desc) }));

function loadCat(): SettingsCat {
  try {
    const v = localStorage.getItem(CAT_KEY);
    if (v && CAT_ORDER.includes(v as SettingsCat)) return v as SettingsCat;
  } catch {
    /* 저장소를 못 쓰면 첫 범주 */
  }
  return "general";
}

function Marked({ segs }: { segs: Segment[] }) {
  return (
    <>
      {segs.map((s, i) => (s.hit ? <mark key={i}>{s.text}</mark> : <span key={i}>{s.text}</span>))}
    </>
  );
}

export function Settings({ onClose, onHooksChanged, onTags, toast }: Props) {
  const [hooks, setHooks] = useState<HookStatus | null>(null);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const about = useAbout();
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const [cat, setCat] = useState<SettingsCat>(loadCat);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [jump, setJump] = useState<{ id: string; block: string; n: number } | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const paneRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef<Partial<Record<SettingsCat, HTMLButtonElement | null>>>({});

  const groups = useMemo(() => searchSettings(ENTRIES, query, CAT_ORDER, CAT_LABELS), [query]);
  const hits = useMemo(() => flatHits(groups), [groups]);
  const searching = query.trim().length > 0;

  const refresh = () => {
    api.hookStatus().then(setHooks).catch((e) => setErr(String(e)));
    api.appInfo().then(setInfo);
    isEnabled().then(setAutostart).catch(() => setAutostart(null));
  };
  useEffect(refresh, []);

  useEffect(() => {
    try {
      localStorage.setItem(CAT_KEY, cat);
    } catch {
      /* 기억 못 해도 동작에는 지장 없음 */
    }
  }, [cat]);

  useEffect(() => setActive(0), [query]);

  // Esc: 검색 중이면 검색만 지우고, 아니면 닫는다 · ⌘F/Ctrl+F: 이 창이 열려 있는 동안은 설정 검색(목록 검색으로 안 감)
  const queryRef = useRef(query);
  queryRef.current = query;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (queryRef.current) {
          setQuery("");
          searchRef.current?.focus();
        } else onClose();
      }
    };
    const onFind = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        e.stopPropagation();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onFind, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onFind, true);
    };
  }, [onClose]);

  // 검색 결과로 이동 — 범주를 바꾼 뒤 그 항목까지 스크롤하고 잠깐 강조
  useEffect(() => {
    if (!jump) return;
    const pane = paneRef.current;
    const el =
      pane?.querySelector<HTMLElement>(`[data-set-id="${jump.id}"]`) ?? pane?.querySelector<HTMLElement>(`[data-set-id="${jump.block}"]`);
    if (!el) return;
    el.scrollIntoView({ block: "start", behavior: "smooth" });
    el.classList.remove("set-flash");
    void el.offsetWidth; // 같은 항목을 다시 골라도 강조가 다시 돌게
    el.classList.add("set-flash");
    if (!el.hasAttribute("tabindex")) el.setAttribute("tabindex", "-1");
    el.focus({ preventScroll: true });
    const t = window.setTimeout(() => el.classList.remove("set-flash"), 1800);
    return () => window.clearTimeout(t);
  }, [jump]);

  const goTo = useCallback((e: SettingsEntry) => {
    setQuery("");
    setCat(e.cat);
    setJump((j) => ({ id: e.id, block: e.block, n: (j?.n ?? 0) + 1 }));
  }, []);

  const pickCat = (c: SettingsCat) => {
    setQuery("");
    setCat(c);
    paneRef.current?.scrollTo({ top: 0 });
  };

  const onTabKey = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    const i = CAT_ORDER.indexOf(cat);
    let next = -1;
    if (e.key === "ArrowDown" || e.key === "ArrowRight") next = (i + 1) % CAT_ORDER.length;
    else if (e.key === "ArrowUp" || e.key === "ArrowLeft") next = (i - 1 + CAT_ORDER.length) % CAT_ORDER.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = CAT_ORDER.length - 1;
    if (next < 0) return;
    e.preventDefault();
    const c = CAT_ORDER[next];
    pickCat(c);
    tabRefs.current[c]?.focus();
  };

  const onSearchKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (!searching) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, Math.max(hits.length - 1, 0)));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const h = hits[active];
      if (h) goTo(h.entry);
    }
  };

  // 검색 결과에서 고른 줄이 보이게
  useEffect(() => {
    if (!searching) return;
    document.getElementById(`set-hit-${active}`)?.scrollIntoView({ block: "nearest" });
  }, [active, searching]);

  const runHooks = async (install: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      const s = install ? await api.installHooks() : await api.uninstallHooks();
      setHooks(s);
      onHooksChanged(s);
      toast(install ? "훅을 설치했습니다" : "훅을 제거했습니다");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const setNum = async (key: string, v: number) => {
    await api.setSetting(key, v);
    api.appInfo().then(setInfo);
  };

  const allOn = hooks && hooks.missing_events.length === 0 && hooks.wake_missing.length === 0 && !hooks.read_missing && !hooks.stale_command;

  /** 블록 → 화면. `Record<BlockId, …>` 라서 settingsIndex 에 블록을 추가하면 여기서 빠진 것을 타입 검사가 잡는다 */
  const BLOCK_VIEW: Record<BlockId, () => ReactNode> = {
    notify: () => (
      <section className="set">
        <h3>알림</h3>
        <label className="check">
          <input type="checkbox" checked={!!info?.notify} onChange={(e) => setNum("notify", e.target.checked ? 1 : 0)} />
          요청이 끝나거나 답을 기다리면 알림 (앱을 보고 있을 때는 띄우지 않음)
        </label>
        <label className="check" data-set-id="notify-body">
          <input
            type="checkbox"
            disabled={!info?.notify}
            checked={!!info?.notify_body}
            onChange={(e) => setNum("notify_body", e.target.checked ? 1 : 0)}
          />
          알림에 응답 첫 줄 보이기 (끄면 제목만 — 잠금 화면에 내용이 안 보임)
        </label>
        <label className="select-row" data-set-id="notify-min">
          <span>이보다 짧게 끝난 요청은 알리지 않기</span>
          <select value={info?.notify_min_sec ?? 0} onChange={(e) => setNum("notify_min_sec", Number(e.target.value))}>
            {NOTIFY_MIN.map((s) => (
              <option key={s} value={s}>
                {s === 0 ? "모두 알림" : s < 60 ? `${s}초` : `${s / 60}분`}
              </option>
            ))}
          </select>
        </label>
      </section>
    ),
    autostart: () => (
      <section className="set">
        <h3>자동 실행</h3>
        <label className="check">
          <input
            type="checkbox"
            disabled={autostart === null}
            checked={!!autostart}
            onChange={async (e) => {
              if (e.target.checked) await enable();
              else await disable();
              setAutostart(await isEnabled());
            }}
          />
          로그인하면 자동으로 실행
        </label>
      </section>
    ),
    update: () => <UpdateSection toast={toast} />,
    hooks: () => (
      <section className="set">
        <h3>Claude Code 훅</h3>
        <Note>
          요청·응답은 대화 기록 파일에서 읽습니다. 훅을 설치하면 작업 완료·권한 대기·세션 종료를 즉시 받아 상태가 더 빨리
          정확해지고, <strong>실행 중인 세션에 입력창·폰에서 말을 넣을 수 있습니다</strong>(세션마다 대기 훅 하나가 쉬는 때를 기다림).
          보낸 이미지를 세션이 권한 창 없이 열 수 있게 <strong>첨부 이미지 폴더 읽기 허용</strong> 규칙 하나도 함께 넣습니다(읽기만, 그
          폴더만). 다른 훅·권한은 건드리지 않고, 설치 전에 <code>settings.json.ai-inbox-backup</code> 을 남깁니다.
        </Note>
        {hooks && (
          <>
            <dl className="kv" data-set-id="hooks-status">
              <dt>상태</dt>
              <dd className={allOn ? "ok" : "warn"}>
                {allOn
                  ? "설치됨"
                  : hooks.installed_events.length === 0
                    ? "설치 안 됨"
                    : hooks.stale_command
                      ? "다른 위치의 앱을 가리킴 — 다시 설치하세요"
                      : hooks.missing_events.length
                        ? `일부만 설치됨 (${hooks.missing_events.join(", ")} 없음)`
                        : "업데이트 필요 — 다시 설치하면 실행 중인 세션에 말을 넣을 수 있습니다"}
              </dd>
              <dt>이벤트</dt>
              <dd>{[...hooks.installed_events, ...hooks.missing_events].map((e) => (hooks.installed_events.includes(e) ? e : `${e}(없음)`)).join(" · ")}</dd>
              <dt>명령어</dt>
              <dd>
                <code>{hooks.command}</code>
              </dd>
              <dt>설정 파일</dt>
              <dd>
                <code>{hooks.settings_path}</code>
              </dd>
              <dt>이미지 읽기</dt>
              <dd>{hooks.installed_events.length === 0 ? "—" : hooks.read_missing ? "허용 규칙 없음 — 다시 설치하세요" : "허용됨(첨부 이미지 폴더만)"}</dd>
              <dt>마지막 수신</dt>
              <dd>{info?.hook_last_at ? fullTime(info.hook_last_at) : "아직 없음"}</dd>
            </dl>
            <div className="set-row" data-set-id="hooks-install">
              <button className="btn primary" disabled={busy} onClick={() => runHooks(true)}>
                {hooks.installed_events.length ? "다시 설치" : "훅 설치"}
              </button>
              <button className="btn" disabled={busy || hooks.installed_events.length === 0} onClick={() => runHooks(false)}>
                제거
              </button>
            </div>
            <p className="set-note small">
              이미 열려 있는 Claude Code 세션도 다시 시작할 필요 없습니다 — 설치 뒤 몇 초 안에 설정을 다시 읽고 연결됩니다.
            </p>
          </>
        )}
        {err && <p className="set-err">{err}</p>}
      </section>
    ),
    codex: () => <CodexSection toast={toast} />,
    collect: () => (
      <section className="set">
        <h3>수집</h3>
        <label className="select-row" data-set-id="collect-backfill">
          <span>처음 볼 때 가져올 기간</span>
          <select value={info?.backfill_days ?? 7} onChange={(e) => setNum("backfill_days", Number(e.target.value))}>
            {BACKFILL.map((d) => (
              <option key={d} value={d}>
                최근 {d}일
              </option>
            ))}
          </select>
        </label>
        <div className="set-row" data-set-id="collect-rescan">
          <button
            className="btn"
            onClick={async () => {
              await api.rescan();
              toast("처음부터 다시 수집합니다 (읽음·별표는 유지)");
            }}
          >
            처음부터 다시 수집
          </button>
        </div>
        <p className="set-note small">
          기간을 바꾸면 "다시 수집" 때 적용됩니다. 그 전에 끝난 요청은 이미 본 것으로 들어갑니다.
        </p>
      </section>
    ),
    clear: () => <ClearSection toast={toast} />,
    tags: () => (
      <section className="set">
        <h3>요청 태그</h3>
        <Note>한 세션에서 여러 주제를 다룰 때 요청마다 주제 태그를 달아 나눠 봅니다. 자동 태깅 규칙은 이 PC 안에서만 돌고, 모델 제안(선택)은 기본 꺼져 있습니다.</Note>
        <div className="set-row">
          <button className="btn" onClick={onTags}>
            태그·자동 규칙 관리…
          </button>
        </div>
      </section>
    ),
    sched: () => <ScheduleSection toast={toast} />,
    history: () => <HistorySection toast={toast} />,
    phone: () => <PhoneSection toast={toast} />,
    "about-version": () => (
      <section className="set">
        <h3>버전</h3>
        {info && (
          <dl className="kv">
            <dt>버전</dt>
            <dd>
              {about ? (
                <>
                  <span className="about-line">{`v${about.version} · 빌드 ${about.build_time} · DB 스키마 v${about.schema_version}`}</span>
                  <CopyReport about={about} toast={toast} />
                </>
              ) : (
                `v${info.version}`
              )}
            </dd>
          </dl>
        )}
      </section>
    ),
    "about-data": () => (
      <section className="set">
        <h3>기록 · 데이터 폴더</h3>
        {info && (
          <dl className="kv">
            <dt>기록</dt>
            <dd>
              세션 {info.sessions.toLocaleString()} · 요청 {info.turns.toLocaleString()} · DB{" "}
              {(info.db_bytes / 1024 / 1024).toFixed(1)}MB
            </dd>
            <dt>데이터</dt>
            <dd>
              <button className="link" onClick={() => api.revealDataDir()}>
                {info.data_dir}
              </button>
            </dd>
            <dt>대화 기록</dt>
            <dd>
              <code>{info.projects_dir}</code>
            </dd>
          </dl>
        )}
      </section>
    ),
  };

  const count = hits.length;
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal settings" role="dialog" aria-modal="true" aria-labelledby="set-title" onClick={(e) => e.stopPropagation()}>
        <header className="modal-head set-head">
          <h2 id="set-title">설정</h2>
          <div className="search set-search">
            <Search size={15} aria-hidden />
            <input
              ref={searchRef}
              type="text"
              role="combobox"
              aria-label="설정 검색"
              aria-expanded={searching}
              aria-controls="set-results"
              aria-autocomplete="list"
              aria-activedescendant={searching && hits[active] ? `set-hit-${active}` : undefined}
              placeholder={`설정 검색 (${kbd("⌘F")})`}
              spellCheck={false}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onSearchKey}
            />
            {query && (
              <button className="icon-btn" title="검색 지우기 (Esc)" onClick={() => (setQuery(""), searchRef.current?.focus())}>
                <X size={14} />
              </button>
            )}
          </div>
          <button className="icon-btn" onClick={onClose} title="닫기">
            <X size={18} />
          </button>
        </header>

        <div className="set-body">
          <nav className="set-nav" aria-label="설정 범주">
            <div role="tablist" aria-orientation="vertical" className="set-tabs">
              {SETTINGS_CATS.map((c) => {
                const Icon = CAT_ICON[c.id];
                const on = c.id === cat;
                return (
                  <button
                    key={c.id}
                    ref={(el) => {
                      tabRefs.current[c.id] = el;
                    }}
                    id={`set-tab-${c.id}`}
                    role="tab"
                    aria-selected={on && !searching}
                    aria-controls={`set-panel-${c.id}`}
                    tabIndex={on ? 0 : -1}
                    className={`set-tab${on && !searching ? " on" : ""}`}
                    title={c.desc}
                    onClick={() => pickCat(c.id)}
                    onKeyDown={onTabKey}
                  >
                    <Icon size={15} aria-hidden />
                    <span>{c.label}</span>
                  </button>
                );
              })}
            </div>
          </nav>

          <div className="set-pane" ref={paneRef}>
            <p className="sr-only" aria-live="polite">
              {searching ? (count ? `설정 ${count}개 항목이 일치합니다` : "일치하는 설정이 없습니다") : ""}
            </p>
            {searching && (
              <div className="set-results">
                {count === 0 ? (
                  <p className="set-empty">
                    「{query.trim()}」와 일치하는 설정이 없습니다. 다른 낱말로 찾아보세요 — 예: 알림, 훅, 예약, 폰, 버전
                  </p>
                ) : (
                  <>
                    <p className="set-count">{count}개 항목</p>
                    <div id="set-results" role="listbox" aria-label="설정 검색 결과">
                      {groups.map((g) => (
                        <div key={g.cat} role="group" aria-label={CAT_LABELS[g.cat as SettingsCat]} className="set-hit-group">
                          <h4>{CAT_LABELS[g.cat as SettingsCat]}</h4>
                          {g.hits.map((h) => {
                            const i = hits.indexOf(h);
                            return (
                              <div
                                key={h.entry.id}
                                id={`set-hit-${i}`}
                                role="option"
                                aria-selected={i === active}
                                className={`set-hit${i === active ? " on" : ""}`}
                                onMouseEnter={() => setActive(i)}
                                onClick={() => goTo(h.entry)}
                              >
                                <span className="set-hit-title">
                                  <Marked segs={h.title} />
                                </span>
                                <span className="set-hit-desc">
                                  <Marked segs={h.desc} />
                                </span>
                                {h.via.length > 0 && <span className="set-hit-via">관련어: {h.via.join(", ")}</span>}
                              </div>
                            );
                          })}
                        </div>
                      ))}
                    </div>
                  </>
                )}
              </div>
            )}
            {/* 범주 화면은 모두 그려 두고 고른 것만 보인다 — 범주를 오가도 입력·QR 같은 상태가 유지되고, 여는 순간의 불러오기도 예전과 같다 */}
            {SETTINGS_CATS.map((c) => (
              <div
                key={c.id}
                id={`set-panel-${c.id}`}
                role="tabpanel"
                aria-labelledby={`set-tab-${c.id}`}
                className="set-panel"
                hidden={searching || c.id !== cat}
              >
                <header className="set-cat-head">
                  <h3>{c.label}</h3>
                  <p>{c.desc}</p>
                </header>
                {SETTINGS_BLOCKS.filter((b) => b.cat === c.id).map((b) => (
                  <div key={b.id} className="set-block" data-set-id={b.id}>
                    {BLOCK_VIEW[b.id]()}
                  </div>
                ))}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

const REPLY_STATE: Record<string, string> = {
  confirm: "데스크톱 확인 대기",
  delivering: "전달 중",
  delivered: "전달됨",
  handled: "처리됨",
  rejected: "보류",
};

function CopyLine({ text, toast }: { text: string; toast: (m: string) => void }) {
  return (
    <div className="copy-line">
      <code>{text}</code>
      <button
        className="icon-btn"
        title="복사"
        onClick={async () => {
          await writeText(text);
          toast("복사했습니다");
        }}
      >
        <Copy size={15} />
      </button>
    </div>
  );
}

const LINK_STATE: Record<string, string> = {
  off: "꺼짐",
  connecting: "중계에 연결하는 중",
  online: "연결됨",
  error: "연결 안 됨",
  upgrade: "업데이트 필요",
};

/** 새 버전 — 알림만 하고, 설치는 사용자가 고를 때 */
function UpdateSection({ toast }: { toast: (m: string) => void }) {
  const [u, setU] = useState<UpdateState | null>(null);
  const [busy, setBusy] = useState(false);
  const load = () => api.updateState().then(setU).catch(() => setU(null));
  useEffect(() => {
    load();
  }, []);
  if (!u) return null;
  return (
    <section className="set">
      <h3>업데이트</h3>
      <dl className="kv">
        <dt>지금 버전</dt>
        <dd>{u.current}</dd>
        <dt>새 버전</dt>
        <dd>
          {u.installed
            ? `${u.installed} 설치됨 — 다시 시작하면 적용`
            : u.available
              ? `${u.available.version} 받을 수 있음 — 목록 아래 알림에서 업데이트`
              : u.last_error
                ? "확인하지 못함"
                : u.last_check
                  ? "최신입니다"
                  : "아직 확인 안 함"}
        </dd>
        {u.last_check && (
          <>
            <dt>마지막 확인</dt>
            <dd>
              {fullTime(u.last_check)}
              {u.last_error ? ` — ${u.last_error}` : ""}
            </dd>
          </>
        )}
      </dl>
      <label className="check" data-set-id="update-check">
        <input
          type="checkbox"
          checked={u.check}
          onChange={async (e) => {
            await api.updateSetCheck(e.target.checked);
            load();
          }}
        />
        새 버전이 나오면 알림 (6시간마다 github.com 의 공개 릴리스 정보만 확인 · 설치는 직접 고를 때만)
      </label>
      <div className="set-row" data-set-id="update-now">
        <button
          className="btn"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              const a = await api.updateCheckNow();
              toast(a ? `새 버전 ${a.version} 이 있습니다` : "최신 버전입니다");
            } catch (e) {
              toast(`확인하지 못했습니다 — ${e}`);
            }
            setBusy(false);
            load();
          }}
        >
          지금 확인
        </button>
      </div>
      <p className="set-note small">받은 업데이트는 앱에 고정된 공개키로 서명을 확인한 뒤에만 설치합니다.</p>
    </section>
  );
}

export function PhoneSection({ toast, autoOffer = false }: { toast: (m: string) => void; autoOffer?: boolean }) {
  const [st, setSt] = useState<RelayStatus | null>(null);
  const [offer, setOffer] = useState<RelayOffer | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const load = () =>
    api
      .relayStatus()
      .then((s) => {
        setSt(s);
        setOffer(s.offer);
      })
      .catch((e) => setErr(String(e)));
  useEffect(() => {
    load();
    const t = window.setInterval(load, 2000);
    return () => window.clearInterval(t);
  }, []);
  // 창을 열자마자 QR 을 보여 준다(아직 연결한 폰이 없을 때)
  const [autoDone, setAutoDone] = useState(false);
  useEffect(() => {
    if (!autoOffer || autoDone || !st || st.offer || st.devices.length > 0) return;
    setAutoDone(true);
    api
      .relayNewOffer()
      .then((o) => {
        setOffer(o);
        load();
      })
      .catch((e) => setErr(String(e)));
  }, [autoOffer, autoDone, st]);
  if (!st) return err ? <p className="set-err">폰 연결 정보를 읽지 못했습니다: {err}</p> : null;

  const set = async (key: RelayKey, value: string) => {
    setErr(null);
    try {
      await api.relaySet(key, value);
      load();
    } catch (e) {
      setErr(String(e));
    }
  };
  const flag = (key: RelayKey, v: boolean) => set(key, v ? "1" : "0");

  return (
    <section className="set">
      <h3>폰 연결 (코노티 앱)</h3>
      <Note>
        코노티 앱의 홈 → AI 작업에서 이 PC 의 세션·요청·문서를 보고, 답을 보내 작업을 이어갑니다. 내용은 PC 와 폰에서만
        풀리는 종단간 암호문으로 오가고, 코노티 서버는 암호문을 넘겨 줄 뿐 읽거나 저장하지 않습니다. 켤 때만 네트워크를
        씁니다.
      </Note>
      <dl className="kv">
        <dt>상태</dt>
        <dd className={st.status.state === "online" ? "ok" : st.status.state === "error" ? "warn" : ""}>
          {st.enabled ? LINK_STATE[st.status.state] ?? st.status.state : "꺼짐"}
          {st.enabled && st.status.state === "online" && st.status.phones_online > 0 && ` · 폰 ${st.status.phones_online}대 보는 중`}
          {st.enabled && st.status.error && st.status.state !== "online" && ` — ${st.status.error}`}
        </dd>
        <dt>서버</dt>
        <dd>
          <select value={st.env} onChange={(e) => set("env", e.target.value)}>
            <option value="prod">코노티 (conoti.app)</option>
            <option value="dev">코노티 개발 서버 (dev.conoti.app)</option>
          </select>
        </dd>
      </dl>
      <label className="check">
        <input type="checkbox" checked={st.enabled} onChange={(e) => flag("enabled", e.target.checked)} />
        폰 연결 켜기
      </label>

      <div data-set-id="phone-pair">
      <div className="set-row">
        <button
          className="btn primary"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            setErr(null);
            try {
              setOffer(await api.relayNewOffer());
              load();
            } catch (e) {
              setErr(String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {st.devices.length ? "다른 폰 연결 (QR)" : "폰 연결하기 (QR)"}
        </button>
      </div>
      {offer && (
        <div className="pair-box">
          <div className="qr" dangerouslySetInnerHTML={{ __html: offer.svg }} />
          <div className="pair-side">
            <p>코노티 앱 → 홈 → AI 작업 → PC 연결하기에서 이 QR 을 찍으세요.</p>
            <p className="set-note small">
              5분 동안 한 번만 쓸 수 있습니다. 폰이 연결을 청하면 이 PC 에서 허용을 한 번 더 눌러야 연결됩니다. QR 을 찍을 수
              없으면 아래 코드를 복사해 앱의 "코드 붙여넣기"에 넣으세요.
            </p>
            <CopyLine text={offer.uri} toast={toast} />
            <button
              className="btn"
              onClick={async () => {
                await api.relayCancelOffer();
                setOffer(null);
              }}
            >
              닫기
            </button>
          </div>
        </div>
      )}
      </div>

      {st.devices.length > 0 && (
        <div data-set-id="phone-devices">
          <h4>연결된 폰</h4>
          <ul className="devices">
            {st.devices.map((d) => (
              <li key={d.pid}>
                <span className="d-name">{d.name}</span>
                <span className="d-time">
                  {d.last_seen ? `마지막 접속 ${fullTime(d.last_seen).slice(5, 16)}` : `연결 ${fullTime(d.created_at).slice(5, 16)}`}
                </span>
                <label className="check inline">
                  <input
                    type="checkbox"
                    checked={d.can_reply}
                    onChange={async (e) => {
                      await api.relaySetDeviceReply(d.pid, e.target.checked);
                      load();
                    }}
                  />
                  답 보내기 허용
                </label>
                <label className="check inline" title="폰에서 보관함 보기·보관·고정·기록에서 지우기. 끄면 보관한 세션은 폰에 보이지 않습니다.">
                  <input
                    type="checkbox"
                    checked={d.can_manage}
                    onChange={async (e) => {
                      await api.relaySetDeviceManage(d.pid, e.target.checked);
                      load();
                    }}
                  />
                  기록 관리 허용
                </label>
                <label className="check inline" title="폰에서 예약 전송을 만들고·고치고·취소하고·처리(보내기/버리기). 예약된 말은 정한 시각에 세션에 자동으로 들어가므로 기본은 꺼져 있습니다. 이 PC 의 AI Inbox 가 켜져 있을 때만 전달됩니다.">
                  <input
                    type="checkbox"
                    checked={d.can_schedule}
                    disabled={!d.can_reply}
                    onChange={async (e) => {
                      await api.relaySetDeviceSchedule(d.pid, e.target.checked);
                      load();
                    }}
                  />
                  예약 허용
                </label>
                <button
                  className="btn"
                  onClick={async () => {
                    await api.relayRemoveDevice(d.pid);
                    toast(`${d.name} 연결을 해제했습니다`);
                    load();
                  }}
                >
                  연결 해제
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div data-set-id="phone-options">
      <label className="check">
        <input type="checkbox" checked={st.push} onChange={(e) => flag("push", e.target.checked)} />
        새 결과가 나오면 폰에 알림 (알림에는 내용 없이 "새 결과가 도착했어요"만 — 폰 앱이 열려 있으면 보내지 않음)
      </label>
      <label className="check">
        <input type="checkbox" checked={st.paused} onChange={(e) => flag("paused", e.target.checked)} />
        폰 답 받기 모두 멈춤
      </label>
      <label className="check">
        <input type="checkbox" checked={st.confirm} onChange={(e) => flag("confirm", e.target.checked)} />
        폰 답을 데스크톱에서 확인한 뒤 전달
      </label>
      <label className="check">
        <input type="checkbox" checked={st.bg_resume} onChange={(e) => flag("bg_resume", e.target.checked)} />
        꺼진 세션은 백그라운드로 이어서 실행 (Claude Code 는 claude --bg --resume · 권한은 기본 설정, Codex 는 codex exec resume · 세션의 샌드박스, 전권이면 낮춤)
      </label>
      </div>

      <div data-set-id="phone-channel">
      <h4>실행 중인 세션에 폰 답 넣기</h4>
      <Note small>
        Claude Code 의 채널 기능(연구 미리보기)을 씁니다. 한 번 등록한 뒤, 세션을 아래 옵션으로 시작하면 폰 답이 그 세션
        대화에 바로 이어집니다. 시작할 때 뜨는 개발 채널 경고에서 1번을 고르세요.
      </Note>
      <CopyLine text={st.mcp_add_command} toast={toast} />
      <CopyLine text={st.start_command} toast={toast} />
      </div>

      {st.replies.length > 0 && (
        <div data-set-id="phone-replies">
          <h4>최근 폰 답</h4>
          <ul className="replies">
            {st.replies.map((r) => (
              <li key={r.reply_id}>
                <span className="r-time">{fullTime(r.received_at).slice(5, 16)}</span>
                <span className="r-who">{r.session_name ?? "?"}</span>
                <span className="r-text">{r.text}</span>
                <span className={`r-state ${r.state}`}>{REPLY_STATE[r.state] ?? r.state}</span>
                {r.note && <span className="r-note">{r.note}</span>}
              </li>
            ))}
          </ul>
        </div>
      )}
      {err && <p className="set-err">{err}</p>}
    </section>
  );
}

/** Codex — 기록(~/.codex/sessions)을 같은 목록에 모은다. 훅·설치가 필요 없다(열려 있는지는 Codex 의 잠금 파일로 본다) */
function CodexSection({ toast }: { toast: (m: string) => void }) {
  const [st, setSt] = useState<CodexStatus | null>(null);
  const load = () => {
    api.codexStatus().then(setSt).catch(() => setSt(null));
  };
  useEffect(load, []);
  return (
    <section className="set">
      <h3>Codex</h3>
      <label className="check">
        <input
          type="checkbox"
          checked={!!st?.enabled}
          disabled={!st}
          onChange={async (e) => {
            await api.setSetting("codex_enabled", e.target.checked ? 1 : 0);
            await api.rescan();
            toast(e.target.checked ? "Codex 세션도 모읍니다" : "Codex 세션을 더 모으지 않습니다 (이미 모은 것은 남습니다)");
            load();
          }}
        />
        OpenAI Codex 세션도 모아 보기
      </label>
      {st && (
        <p className="set-note small">
          {st.found ? `기록 폴더 ${st.sessions_dir} · 모은 세션 ${st.sessions}개` : `기록 폴더(${st.sessions_dir})가 아직 없습니다`}
          {st.live != null && st.live > 0 ? ` · 지금 열려 있는 세션 ${st.live}개` : ""}
          <br />
          {st.bin
            ? `codex ${st.version ?? ""} — ${st.bin}`
            : "codex 실행 파일을 찾지 못했습니다 — 모아 보기는 되지만, 말 넣기·새 작업은 Codex CLI 가 있어야 합니다"}
        </p>
      )}
      <Note small>
        훅 설치가 필요 없습니다. 열려 있는 Codex 세션(터미널·앱)에는 보낸 말이 Codex 대기열로 바로 들어가고, 일하는 중이면 끝난 뒤에 들어갑니다.
        꺼진 세션은 codex exec 로 이어서 실행합니다 — 승인은 묻지 않고 그 세션이 쓰던 샌드박스를 씁니다(전권이던 세션은 폴더 안 쓰기로
        낮춤 · 폰 답은 아래 "꺼진 세션 이어서 실행"을 켰을 때만).
      </Note>
    </section>
  );
}
