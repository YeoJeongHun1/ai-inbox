import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Archive, ArchiveRestore, Image as ImageIcon, Layers, MessageSquare, Search, Smartphone, Trash2, X } from "lucide-react";
import { api, filterActive, NO_FILTER, type TagFilter, type ArchiveImage, type ArchiveStats, type SentMessage, type SessionRow, type SessionScope, type TurnHit } from "../api";
import { fullTime, listTime } from "../format";
import { AttStrip, AttThumb, Lightbox, forgetAtt, sizeText } from "./Attachments";
import { TagFilterChips, TagLabels } from "./TagUi";

type Tab = "turns" | "sessions" | "messages" | "images";
type Device = "all" | "desktop" | "phone";

interface Props {
  onClose: () => void;
  /** 대화에서 보기 — 세션을 열고 그 요청 문서를 띄운다 */
  onOpenTurn: (sessionId: string, turnId: number | null) => void;
  /** 세션을 지웠다 — 열려 있던 대화면 닫는다 */
  onSessionsDeleted: (ids: string[]) => void;
  toast: (m: string) => void;
}

const IDLE: { days: number | null; label: string }[] = [
  { days: null, label: "언제든" },
  { days: 7, label: "7일 넘게 조용함" },
  { days: 30, label: "30일 넘게 조용함" },
  { days: 90, label: "90일 넘게 조용함" },
];

const STATE: Record<SentMessage["state"], string> = {
  confirm: "PC 확인 대기",
  delivering: "보내는 중",
  delivered: "전달됨",
  handled: "처리됨",
  rejected: "보내지 못함",
};

/** 검색어를 굵게 — 글자 그대로 비교(정규식 아님) */
function Hit({ text, q }: { text: string; q: string }) {
  const needle = q.trim().toLowerCase();
  if (!needle) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  const lower = text.toLowerCase();
  let at = 0;
  for (let i = lower.indexOf(needle); i >= 0 && parts.length < 40; i = lower.indexOf(needle, at)) {
    if (i > at) parts.push(text.slice(at, i));
    parts.push(<mark key={i}>{text.slice(i, i + needle.length)}</mark>);
    at = i + needle.length;
  }
  parts.push(text.slice(at));
  return <>{parts}</>;
}

/** 지우기 전 한 번 더 — 네이티브 확인 창을 띄우지 않는다 */
function ConfirmButton({ label, confirmLabel, disabled, onConfirm }: { label: string; confirmLabel: string; disabled?: boolean; onConfirm: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <button
      className={`btn ${armed ? "danger" : ""}`}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        if (armed) {
          setArmed(false);
          onConfirm();
        } else setArmed(true);
      }}
    >
      <Trash2 size={14} /> {armed ? confirmLabel : label}
    </button>
  );
}

export function ArchiveView({ onClose, onOpenTurn, onSessionsDeleted, toast }: Props) {
  const [tab, setTab] = useState<Tab>("turns");
  const [query, setQuery] = useState("");
  const [q, setQ] = useState(""); // 입력이 멈춘 뒤의 검색어
  const [device, setDevice] = useState<Device>("all");
  const [imagesOnly, setImagesOnly] = useState(false);
  const [turns, setTurns] = useState<TurnHit[]>([]);
  /** 대화 검색을 태그로 거른다(검색어와 함께 · 검색어 없이 태그만으로도) */
  const [tagFilter, setTagFilter] = useState<TagFilter>(NO_FILTER);
  const tagKey = JSON.stringify(tagFilter);
  const [messages, setMessages] = useState<SentMessage[]>([]);
  const [images, setImages] = useState<ArchiveImage[]>([]);
  const [rows, setRows] = useState<SessionRow[]>([]);
  const [scope, setScope] = useState<SessionScope>("visible");
  const [shortOnly, setShortOnly] = useState(false);
  const [idleDays, setIdleDays] = useState<number | null>(null);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [stats, setStats] = useState<ArchiveStats | null>(null);
  const [viewer, setViewer] = useState<{ ids: string[]; index: number } | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const loadSeq = useRef(0);

  useEffect(() => {
    input.current?.focus();
  }, []);

  useEffect(() => {
    const t = window.setTimeout(() => setQ(query), 220);
    return () => window.clearTimeout(t);
  }, [query]);

  const refreshStats = useCallback(() => {
    api.archiveStats().then(setStats).catch(() => setStats(null));
  }, []);
  useEffect(refreshStats, [refreshStats]);

  /** 처음부터(append=false) 또는 이어서 불러오기 */
  const load = useCallback(
    async (append: boolean) => {
      const my = ++loadSeq.current;
      setLoading(true);
      try {
        if (tab === "turns") {
          const before = append ? (turns[turns.length - 1]?.prompt_at ?? null) : null;
          const p = await api.archiveTurns(q, before, tagFilter);
          if (my !== loadSeq.current) return;
          setTurns((cur) => (append ? [...cur, ...p.items] : p.items));
          setMore(p.has_more);
        } else if (tab === "sessions") {
          const p = await api.archiveSessions(q, scope, shortOnly, idleDays, append ? rows.length : 0);
          if (my !== loadSeq.current) return;
          setRows((cur) => (append ? [...cur, ...p.items] : p.items));
          setMore(p.has_more);
        } else if (tab === "messages") {
          const before = append ? (messages[messages.length - 1]?.at ?? null) : null;
          const p = await api.archiveMessages(q, device, imagesOnly, before);
          if (my !== loadSeq.current) return;
          setMessages((cur) => (append ? [...cur, ...p.items] : p.items));
          setMore(p.has_more);
        } else {
          const before = append ? (images[images.length - 1]?.created_at ?? null) : null;
          const p = await api.archiveImages(q, before);
          if (my !== loadSeq.current) return;
          setImages((cur) => (append ? [...cur, ...p.items] : p.items));
          setMore(p.has_more);
        }
      } catch (e) {
        toast(String(e));
      } finally {
        if (my === loadSeq.current) setLoading(false);
      }
    },
    [tab, q, device, imagesOnly, turns, messages, images, rows, scope, shortOnly, idleDays, toast, tagFilter],
  );

  // 탭·검색어·필터가 바뀌면 처음부터
  useEffect(() => {
    setSelected(new Set());
    load(false);
  }, [tab, q, device, imagesOnly, scope, shortOnly, idleDays, tagKey]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !viewer) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, viewer]);

  const toggle = (key: string) =>
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const deleteMessages = async (rids: string[]) => {
    try {
      const n = await api.archiveDeleteMessages(rids);
      toast(`메시지 ${n}개를 지웠습니다`);
      setMessages((cur) => cur.filter((m) => !rids.includes(m.rid)));
      setSelected(new Set());
      refreshStats();
    } catch (e) {
      toast(String(e));
    }
  };

  const deleteImages = async (ids: string[]) => {
    try {
      const n = await api.archiveDeleteImages(ids);
      ids.forEach(forgetAtt);
      toast(`이미지 ${n}장을 지웠습니다`);
      setImages((cur) => cur.filter((m) => !ids.includes(m.id)));
      setSelected(new Set());
      refreshStats();
    } catch (e) {
      toast(String(e));
    }
  };

  const setArchived = async (ids: string[], archived: boolean) => {
    try {
      const n = await api.archiveSetArchived(ids, archived);
      toast(archived ? `세션 ${n}개를 보관했습니다` : `세션 ${n}개를 목록으로 되돌렸습니다`);
      setSelected(new Set());
      refreshStats();
      load(false);
    } catch (e) {
      toast(String(e));
    }
  };

  const tidy = async (kind: "short" | "idle") => {
    try {
      const n = await api.archiveTidy(kind);
      toast(`세션 ${n}개를 보관했습니다 — 보관함에서 되돌릴 수 있습니다`);
      setSelected(new Set());
      refreshStats();
      load(false);
    } catch (e) {
      toast(String(e));
    }
  };

  const deleteSessions = async (ids: string[]) => {
    try {
      const { deleted, skipped } = await api.archiveDeleteSessions(ids);
      const kept = skipped.length ? ` · ${skipped.length}개는 작업 중이거나 보낼 말이 남아 두었습니다(${skipped.slice(0, 3).join(", ")}${skipped.length > 3 ? " …" : ""})` : "";
      toast(`세션 ${deleted.length}개를 기록에서 지웠습니다${kept}`);
      onSessionsDeleted(deleted);
      setRows((cur) => cur.filter((r) => !deleted.includes(r.id)));
      setSelected(new Set());
      refreshStats();
    } catch (e) {
      toast(String(e));
    }
  };

  const selRows = rows.filter((r) => selected.has(r.id));
  const allOn = rows.length > 0 && rows.every((r) => selected.has(r.id));
  const filtered = shortOnly || idleDays !== null || q.trim() !== "";

  const imageIds = useMemo(() => images.map((m) => m.id), [images]);
  const openImages = (ids: string[], index: number) => setViewer({ ids, index });

  const TABS: { key: Tab; label: string; icon: React.ReactNode }[] = [
    { key: "turns", label: "대화 검색", icon: <Search size={14} /> },
    { key: "sessions", label: "세션", icon: <Layers size={14} /> },
    { key: "messages", label: "보낸 메시지", icon: <MessageSquare size={14} /> },
    { key: "images", label: "이미지", icon: <ImageIcon size={14} /> },
  ];

  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="archive">
        <header className="archive-head">
          <h2>기록</h2>
          <label className="search archive-search">
            <Search size={15} />
            <input
              ref={input}
              value={query}
              spellCheck={false}
              placeholder={
                tab === "turns"
                  ? "모든 세션의 요청·요약·응답에서 찾기"
                  : tab === "sessions"
                    ? "세션 이름·프로젝트·요청에서 찾기"
                    : tab === "messages"
                      ? "보낸 글·세션 이름에서 찾기"
                      : "파일 이름·함께 보낸 글·세션 이름에서 찾기"
              }
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <button className="icon-btn" title="닫기 (Esc)" onClick={onClose}>
            <X size={18} />
          </button>
        </header>

        <nav className="archive-tabs">
          {TABS.map((t) => (
            <button key={t.key} className={`tab ${tab === t.key ? "on" : ""}`} onClick={() => setTab(t.key)}>
              {t.icon}
              {t.label}
            </button>
          ))}
          {stats && (
            <span className="archive-stats">
              {tab === "sessions"
                ? `세션 ${stats.sessions} · 보관 ${stats.archived}`
                : `보낸 메시지 ${stats.messages} · 이미지 ${stats.images}장 · ${sizeText(stats.image_bytes)}`}
            </span>
          )}
        </nav>

        {tab === "sessions" && (
          <>
            <div className="archive-tools">
              <div className="seg">
                {(["visible", "archived", "all"] as SessionScope[]).map((k) => (
                  <button key={k} className={scope === k ? "on" : ""} onClick={() => setScope(k)}>
                    {k === "visible" ? "목록에 있음" : k === "archived" ? "보관함" : "전체"}
                  </button>
                ))}
              </div>
              <label className="check">
                <input type="checkbox" checked={shortOnly} onChange={(e) => setShortOnly(e.target.checked)} />
                요청 1개 이하
              </label>
              <select className="idle-pick" value={idleDays ?? ""} onChange={(e) => setIdleDays(e.target.value ? Number(e.target.value) : null)} title="마지막 활동">
                {IDLE.map((o) => (
                  <option key={o.label} value={o.days ?? ""}>
                    {o.label}
                  </option>
                ))}
              </select>
              <span className="grow" />
              {selRows.length > 0 && (
                <>
                  {selRows.some((r) => !r.archived) && (
                    <button className="btn" onClick={() => setArchived(selRows.filter((r) => !r.archived).map((r) => r.id), true)}>
                      <Archive size={14} /> 보관
                    </button>
                  )}
                  {selRows.some((r) => r.archived) && (
                    <button className="btn" onClick={() => setArchived(selRows.filter((r) => r.archived).map((r) => r.id), false)}>
                      <ArchiveRestore size={14} /> 목록으로
                    </button>
                  )}
                  <ConfirmButton
                    label={`${selRows.length}개 지우기`}
                    confirmLabel={`정말 ${selRows.length}개 지우기`}
                    onConfirm={() => deleteSessions(selRows.map((r) => r.id))}
                  />
                </>
              )}
            </div>
            {scope === "visible" && !filtered && stats && (stats.tidy_short > 0 || stats.tidy_idle > 0) && (
              <div className="tidy-bar">
                <span className="tidy-title">정리 제안</span>
                {stats.tidy_short > 0 && (
                  <button className="more" onClick={() => tidy("short")}>
                    요청 1개 이하 {stats.tidy_short}개 보관
                  </button>
                )}
                {stats.tidy_idle > 0 && (
                  <button className="more" onClick={() => tidy("idle")}>
                    30일 넘게 조용한 {stats.tidy_idle}개 보관
                  </button>
                )}
                <span className="set-note small">고정·실행 중·안 읽은 결과가 있는 세션은 빼고 보관합니다.</span>
              </div>
            )}
          </>
        )}
        {tab === "messages" && (
          <div className="archive-tools">
            <div className="seg">
              {(["all", "desktop", "phone"] as Device[]).map((d) => (
                <button key={d} className={device === d ? "on" : ""} onClick={() => setDevice(d)}>
                  {d === "all" ? "전체" : d === "desktop" ? "PC 에서" : "폰에서"}
                </button>
              ))}
            </div>
            <label className="check">
              <input type="checkbox" checked={imagesOnly} onChange={(e) => setImagesOnly(e.target.checked)} />
              이미지 있는 것만
            </label>
            <span className="grow" />
            {selected.size > 0 && (
              <ConfirmButton
                label={`선택 ${selected.size}개 지우기`}
                confirmLabel={`정말 ${selected.size}개 지우기`}
                onConfirm={() => deleteMessages([...selected])}
              />
            )}
          </div>
        )}
        {tab === "images" && (
          <div className="archive-tools">
            <span className="set-note small">
              보낸 이미지는 이 PC 의 앱 데이터 폴더에만 있습니다. 지우면 파일이 사라지고, 그 이미지를 붙인 메시지에서도 빠집니다.
            </span>
            <span className="grow" />
            {selected.size > 0 && (
              <ConfirmButton label={`선택 ${selected.size}장 지우기`} confirmLabel={`정말 ${selected.size}장 지우기`} onConfirm={() => deleteImages([...selected])} />
            )}
          </div>
        )}

        <div className="archive-body">
          {tab === "turns" && <TagFilterChips filter={tagFilter} onFilter={setTagFilter} className="archive-tags" />}
          {tab === "turns" &&
            (q.trim() === "" && !filterActive(tagFilter) ? (
              <p className="archive-empty">검색어를 입력하면 모든 세션의 요청·작업 요약·응답에서 찾습니다. 위 태그를 고르면 그 태그의 요청만 모아 봅니다. 결과를 누르면 그 대화로 갑니다.</p>
            ) : (
              <ul className="hit-list">
                {turns.map((h) => (
                  <li
                    key={h.turn_id}
                    className="hit"
                    onClick={() => {
                      onOpenTurn(h.session_id, h.turn_id);
                      onClose();
                    }}
                  >
                    <div className="hit-head">
                      <strong>{h.session_name}</strong>
                      {h.archived && <span className="s-tag">보관됨</span>}
                      <span className="hit-where">{h.snippet_in === "prompt" ? "요청" : h.snippet_in === "summary" ? "작업 요약" : h.snippet_in === "response" ? "응답" : "세션"}</span>
                      <time title={fullTime(h.prompt_at)}>{listTime(h.prompt_at)}</time>
                    </div>
                    <TagLabels tags={h.tags ?? []} />
                    {h.snippet_in !== "prompt" && h.prompt && <div className="hit-prompt">{h.prompt}</div>}
                    <div className="hit-snippet">
                      <Hit text={h.snippet} q={q} />
                    </div>
                    <AttStrip ids={h.atts} px={44} onOpen={openImages} />
                  </li>
                ))}
                {!loading && turns.length === 0 && <li className="archive-empty">찾은 대화가 없습니다.</li>}
              </ul>
            ))}

          {tab === "sessions" && (
            <>
              <p className="archive-note">
                <strong>보관</strong>하면 목록과 폰에서 빠지고 대화 검색에는 남습니다. 보관한 뒤에 새 요청이나 결과가 오면 저절로 목록으로 돌아옵니다.{" "}
                <strong>지우기</strong>는 이 앱에 모은 요청·보낸 메시지·이미지를 지웁니다(작업 중인 세션은 남깁니다). Claude Code·Codex 의 원본 대화 기록(~/.claude · ~/.codex)은 그대로입니다.
              </p>
              {rows.length > 0 && (
                <label className="check srow-all">
                  <input
                    type="checkbox"
                    checked={allOn}
                    onChange={() => setSelected(allOn ? new Set() : new Set(rows.map((r) => r.id)))}
                  />
                  보이는 {rows.length}개 모두 선택
                </label>
              )}
              <ul className="msg-list">
                {rows.map((r) => (
                  <li key={r.id} className={`msg srow ${selected.has(r.id) ? "sel" : ""}`}>
                    <input type="checkbox" checked={selected.has(r.id)} onChange={() => toggle(r.id)} title="선택" />
                    <div className="msg-body">
                      <div className="hit-head">
                        <strong>
                          <Hit text={r.name} q={q} />
                        </strong>
                        {r.project_name && <span className="hit-where">{r.project_name}</span>}
                        {r.archived && <span className="s-tag">보관됨</span>}
                        {r.pinned && <span className="s-tag">고정</span>}
                        {r.live && <span className="s-tag">실행 중</span>}
                        {r.active > 0 && <span className="s-tag">작업 중</span>}
                        {r.unread > 0 && <span className="s-tag unread">안 읽음 {r.unread}</span>}
                        <time title={r.last_at ? fullTime(r.last_at) : ""}>{r.last_at ? listTime(r.last_at) : "—"}</time>
                      </div>
                      <div className="srow-meta">
                        요청 {r.turns}개{r.first_at && ` · ${fullTime(r.first_at).slice(0, 16)} 시작`}
                      </div>
                      <div className="msg-actions">
                        <button
                          className="more"
                          onClick={() => {
                            onOpenTurn(r.id, null);
                            onClose();
                          }}
                        >
                          대화 열기
                        </button>
                        <button className="more" onClick={() => setArchived([r.id], !r.archived)}>
                          {r.archived ? "목록으로 되돌리기" : "보관"}
                        </button>
                        <ConfirmButton label="지우기" confirmLabel="정말 지우기" onConfirm={() => deleteSessions([r.id])} />
                      </div>
                    </div>
                  </li>
                ))}
                {!loading && rows.length === 0 && (
                  <li className="archive-empty">
                    {filtered ? "조건에 맞는 세션이 없습니다." : scope === "archived" ? "보관한 세션이 없습니다." : "세션이 없습니다."}
                  </li>
                )}
              </ul>
            </>
          )}

          {tab === "messages" && (
            <ul className="msg-list">
              {messages.map((m) => (
                <li key={m.rid} className={`msg ${selected.has(m.rid) ? "sel" : ""}`}>
                  <input type="checkbox" checked={selected.has(m.rid)} onChange={() => toggle(m.rid)} title="선택" />
                  <div className="msg-body">
                    <div className="hit-head">
                      <strong>{m.session_name ?? "지워진 세션"}</strong>
                      <span className="hit-where">
                        {m.from_phone ? (
                          <>
                            <Smartphone size={11} /> {m.device_name ?? "폰"}
                          </>
                        ) : (
                          "PC"
                        )}
                      </span>
                      <span className={`msg-state s-${m.state}`} title={m.note ?? ""}>
                        {STATE[m.state]}
                      </span>
                      <time title={fullTime(m.at)}>{listTime(m.at)}</time>
                    </div>
                    {m.text ? (
                      <div className="msg-text">
                        <Hit text={m.text} q={q} />
                      </div>
                    ) : (
                      <div className="msg-text muted">(이미지만 보냄)</div>
                    )}
                    <AttStrip ids={m.atts.map((a) => a.id)} px={52} onOpen={openImages} />
                    <div className="msg-actions">
                      {m.session_id && (
                        <button
                          className="more"
                          onClick={() => {
                            onOpenTurn(m.session_id!, m.result_turn);
                            onClose();
                          }}
                        >
                          대화에서 보기
                        </button>
                      )}
                      <ConfirmButton label="지우기" confirmLabel="정말 지우기" onConfirm={() => deleteMessages([m.rid])} />
                    </div>
                  </div>
                </li>
              ))}
              {!loading && messages.length === 0 && <li className="archive-empty">{q ? "찾은 메시지가 없습니다." : "아직 보낸 메시지가 없습니다."}</li>}
            </ul>
          )}

          {tab === "images" && (
            <>
              <div className="img-grid">
                {images.map((m, i) => {
                  const use = m.used_in[0];
                  return (
                    <figure key={m.id} className={`img-cell ${selected.has(m.id) ? "sel" : ""}`}>
                      <AttThumb id={m.id} px={136} title={m.name ?? "크게 보기"} onClick={() => openImages(imageIds, i)} />
                      <label className="img-pick" title="선택">
                        <input type="checkbox" checked={selected.has(m.id)} onChange={() => toggle(m.id)} />
                      </label>
                      <figcaption>
                        <span className="img-where">{use ? (use.session_name ?? "지워진 세션") : "—"}</span>
                        <span className="img-sub">
                          {listTime(m.created_at)} · {m.source === "phone" ? "폰" : "PC"} · {sizeText(m.bytes)}
                          {m.used_in.length > 1 && ` · ${m.used_in.length}번 보냄`}
                        </span>
                      </figcaption>
                    </figure>
                  );
                })}
              </div>
              {!loading && images.length === 0 && <p className="archive-empty">{q ? "찾은 이미지가 없습니다." : "아직 보낸 이미지가 없습니다."}</p>}
            </>
          )}

          {more && (
            <button className="load-older" disabled={loading} onClick={() => load(true)}>
              {loading ? "불러오는 중…" : "더 보기"}
            </button>
          )}
        </div>
      </div>

      {viewer && (
        <Lightbox
          ids={viewer.ids}
          index={viewer.index}
          toast={toast}
          onClose={() => setViewer(null)}
          onDelete={async (id) => {
            await api.archiveDeleteImages([id]);
            setImages((cur) => cur.filter((x) => x.id !== id));
            setMessages((cur) => cur.map((m) => ({ ...m, atts: m.atts.filter((a) => a.id !== id) })));
            setTurns((cur) => cur.map((t) => ({ ...t, atts: t.atts.filter((a) => a !== id) })));
            setViewer((v) => {
              if (!v) return v;
              const ids = v.ids.filter((x) => x !== id);
              return ids.length ? { ids, index: Math.min(v.index, ids.length - 1) } : null;
            });
            refreshStats();
          }}
        />
      )}
    </div>
  );
}
