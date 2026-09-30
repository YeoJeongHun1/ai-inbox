import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { announceRead, api, type ReadBatch, type ToastAction, type Counts, type Filter, type HookStatus, type SessionItem, type UpdateState } from "./api";
import { fullTime } from "./format";
import { kbd } from "./keys";
import { Sidebar, type SessionAction } from "./components/Sidebar";
import { ChatView } from "./components/ChatView";
import { DocPanel } from "./components/DocPanel";
import { Settings } from "./components/Settings";
import { PairDialog } from "./components/PairDialog";
import { PhoneDialog } from "./components/PhoneDialog";
import { NewTaskDialog } from "./components/NewTaskDialog";
import { ScheduleList } from "./components/ScheduleList";
import { ArchiveView } from "./components/ArchiveView";
import { ClearDialog } from "./components/ClearDialog";
import { HistoryChat } from "./components/HistoryChat";
import { TagManager } from "./components/TagManager";
import { UpdateBar, WhatsNew } from "./components/UpdateBar";
import "./App.css";

interface ChangedPayload {
  sessions: string[];
  counts: Counts;
  working: boolean;
}

function hookLineOf(h: HookStatus | null, lastAt: string | null): { ok: boolean; text: string } {
  if (!h) return { ok: false, text: "훅 상태 확인 중" };
  if (h.installed_events.length === 0) return { ok: false, text: "훅 미설치 — 설정에서 설치" };
  if (h.stale_command) return { ok: false, text: "훅이 다른 위치의 앱을 가리킴" };
  if (h.missing_events.length) return { ok: false, text: "훅 일부만 설치됨" };
  if (h.wake_missing.length || h.read_missing) return { ok: false, text: "훅 업데이트 필요 — 설정에서 다시 설치" };
  return { ok: true, text: lastAt ? `훅 연결됨 · 마지막 ${fullTime(lastAt).slice(11, 16)}` : "훅 연결됨" };
}

export default function App() {
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [openTurn, setOpenTurn] = useState<number | null>(null);
  const [counts, setCounts] = useState<Counts>({ unread: 0, attention: 0, active: 0, kept: 0, undecided: 0 });
  const [chatKey, setChatKey] = useState(0);
  const [docKey, setDocKey] = useState(0);
  const [working, setWorking] = useState(false);
  const [settings, setSettings] = useState(false);
  const [pair, setPair] = useState<{ name: string; sas: string } | null>(null);
  const [phoneOpen, setPhoneOpen] = useState(false);
  const [newTask, setNewTask] = useState(false);
  /** 예약 목록 창 · 걸려 있는/처리 대기 예약 수 */
  const [schedOpen, setSchedOpen] = useState(false);
  const [schedCount, setSchedCount] = useState({ active: 0, held: 0 });
  useEffect(() => {
    const load = () => api.schedCounts().then(setSchedCount).catch(() => {});
    load();
    const un = listen("sched-changed", load);
    const t = setInterval(load, 60_000);
    return () => {
      clearInterval(t);
      un.then((f) => f());
    };
  }, []);
  const [archive, setArchive] = useState(false);
  /** 대화 이력 찾기(검색 모드 채팅)를 메인에 띄웠나 */
  const [historyChat, setHistoryChat] = useState(false);
  /** 요청 태그 관리 창 */
  const [tagMgr, setTagMgr] = useState(false);
  /** /clear 로 끝난 대화 결정 창 */
  const [clearDlg, setClearDlg] = useState<{ onlyUndecided: boolean } | null>(null);
  /** 방금 띄운 새 작업(짧은 ID) — 목록에 나타나면 연다 */
  const openWhenListed = useRef<{ short: string; until: number } | null>(null);
  const [phone, setPhone] = useState<{ devices: number; online: number } | null>(null);
  const [relayUpgrade, setRelayUpgrade] = useState<string | null>(null);
  const [upd, setUpd] = useState<UpdateState | null>(null);
  const [updProgress, setUpdProgress] = useState<{ done: number; total: number | null } | null>(null);
  const [whatsNew, setWhatsNew] = useState<{ version: string; notes: string } | null>(null);
  const loadUpdate = useCallback(() => {
    api
      .updateState()
      .then((u) => {
        setUpd(u);
        if (u.whats_new) setWhatsNew((cur) => cur ?? { version: u.current, notes: u.whats_new! });
      })
      .catch(() => setUpd(null));
  }, []);
  useEffect(() => {
    loadUpdate();
    const u1 = listen("update-available", loadUpdate);
    const u2 = listen("update-installed", loadUpdate);
    const u3 = listen<{ done: number; total: number | null }>("update-progress", (e) => setUpdProgress(e.payload));
    return () => {
      u1.then((f) => f());
      u2.then((f) => f());
      u3.then((f) => f());
    };
  }, [loadUpdate]);
  useEffect(() => {
    const load = () =>
      api
        .relayStatus()
        .then((s) => {
          setPhone({ devices: s.devices.length, online: s.enabled ? s.status.phones_online : 0 });
          setRelayUpgrade(s.enabled && s.status.state === "upgrade" ? s.status.error : null);
        })
        .catch(() => setPhone(null));
    load();
    const t = window.setInterval(load, 5000);
    return () => window.clearInterval(t);
  }, []);
  const [hooks, setHooks] = useState<HookStatus | null>(null);
  const [hookLast, setHookLast] = useState<string | null>(null);
  const [toastMsg, setToastMsg] = useState<{ msg: string; action?: ToastAction } | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;
  const listTimer = useRef<number | undefined>(undefined);

  const toast = useCallback((msg: string, action?: ToastAction) => {
    const t = { msg, action };
    setToastMsg(t);
    window.setTimeout(() => setToastMsg((cur) => (cur === t ? null : cur)), action ? 7000 : 2200);
  }, []);

  const loadList = useCallback(async () => {
    const [list, c] = await Promise.all([api.listSessions(filter, query), api.counts()]);
    setSessions(list);
    setCounts(c);
    const want = openWhenListed.current;
    const hit = want && Date.now() < want.until ? list.find((x) => x.id.startsWith(`${want.short}-`)) : undefined;
    if (hit) {
      openWhenListed.current = null;
      setOpenTurn(null);
      setSelected(hit.id);
    } else {
      setSelected((cur) => cur ?? list[0]?.id ?? null);
    }
  }, [filter, query]);

  useEffect(() => {
    window.clearTimeout(listTimer.current);
    listTimer.current = window.setTimeout(loadList, query ? 180 : 0);
  }, [loadList, query]);

  const refreshHooks = useCallback(() => {
    api.hookStatus().then(setHooks).catch(() => setHooks(null));
    api.appInfo().then((i) => setHookLast(i.hook_last_at));
  }, []);
  useEffect(refreshHooks, [refreshHooks]);

  // 수집 스레드가 바뀐 세션을 알려 준다
  useEffect(() => {
    const un1 = listen<ChangedPayload>("inbox-changed", (e) => {
      setWorking(e.payload.working);
      window.clearTimeout(listTimer.current);
      listTimer.current = window.setTimeout(loadList, 250);
      const sel = selectedRef.current;
      if (!e.payload.sessions.length || (sel && e.payload.sessions.includes(sel))) {
        setChatKey((k) => k + 1);
        setDocKey((k) => k + 1);
      }
    });
    const un2 = listen<Counts>("inbox-counts", (e) => setCounts(e.payload));
    // 폰이 연결을 청하면 허용 창
    const un3 = listen<{ name: string; sas: string }>("relay-pair", (e) => setPair(e.payload));
    const un4 = listen("open-phone", () => setPhoneOpen(true));
    // /clear 로 세션이 끝났다 — 기본 처리(설정)를 알리고 이력으로 남길지 묻는다
    const un5 = listen<{ count: number; default: string }>("clear-detected", (e) => {
      const d = e.payload.default;
      const msg =
        d === "keep"
          ? `/clear 된 대화 ${e.payload.count}개를 이력으로 보관했습니다`
          : d === "ask"
            ? `/clear 된 대화 ${e.payload.count}개 — 이력으로 남길지 정해 주세요(정할 때까지 지우지 않습니다)`
            : `/clear 된 대화 ${e.payload.count}개는 삭제 예약됩니다 — 이력으로 남길까요?`;
      toast(msg, { label: "정하기", run: () => setClearDlg({ onlyUndecided: true }) });
    });
    const hookTimer = window.setInterval(() => api.appInfo().then((i) => setHookLast(i.hook_last_at)), 30_000);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      un4.then((f) => f());
      un5.then((f) => f());
      window.clearInterval(hookTimer);
    };
  }, [loadList, toast]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "h") {
        e.preventDefault();
        setHistoryChat((v) => !v);
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setArchive(true);
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
      if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        setSettings(true);
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        setNewTask(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const onRead = useCallback(() => {
    window.clearTimeout(listTimer.current);
    listTimer.current = window.setTimeout(loadList, 120);
  }, [loadList]);

  const onDocChanged = useCallback(() => {
    onRead();
    setChatKey((k) => k + 1);
  }, [onRead]);

  const select = (id: string) => {
    if (id !== selected) setOpenTurn(null);
    setHistoryChat(false);
    setSelected(id);
  };

  const onGone = useCallback(() => {
    setSelected(null);
    setOpenTurn(null);
    toast("이 세션은 기록에서 지워졌습니다");
    onRead();
  }, [toast, onRead]);

  /** 지운 세션을 보고 있었으면 닫는다(목록을 다시 읽으면 첫 세션이 열린다) */
  const forgetSessions = (ids: string[]) => {
    if (selectedRef.current && ids.includes(selectedRef.current)) {
      setSelected(null);
      setOpenTurn(null);
    }
    onRead();
  };

  /** 모두 읽음(세션 하나 또는 전부) — 되돌리기 버튼이 달린 알림을 띄운다 */
  const readAll = useCallback(
    async (sessionId: string | null, label: string) => {
      const b: ReadBatch = await api.markSessionRead(sessionId);
      announceRead(b.ids, b.at);
      setChatKey((k) => k + 1);
      onRead();
      if (!b.ids.length) return;
      toast(`${label} — 안 읽은 결과 ${b.ids.length}개를 읽음으로 표시했습니다`, {
        label: "되돌리기",
        run: async () => {
          await api.restoreUnread(b);
          announceRead(b.ids, null);
          setChatKey((k) => k + 1);
          onRead();
        },
      });
    },
    [onRead, toast],
  );

  const onSessionAction = async (id: string, a: SessionAction) => {
    try {
      if (a === "pin" || a === "unpin") await api.setPinned(id, a === "pin");
      else if (a === "read") {
        await readAll(id, sessions.find((s) => s.id === id)?.name ?? "세션");
        return;
      }
      else if (a === "archive") {
        await api.setHidden(id, true);
        toast(`보관했습니다 — 기록(${kbd("⌘⇧F")}) › 세션 › 보관함에서 되돌릴 수 있습니다`);
      } else if (a === "delete") {
        const { deleted } = await api.archiveDeleteSessions([id]);
        if (deleted.length) {
          toast("기록에서 지웠습니다");
          forgetSessions(deleted);
        } else {
          toast("작업 중이거나 보낼 말이 남은 세션은 끝난 뒤에 지울 수 있습니다");
        }
        return;
      }
      if (id === selectedRef.current) setChatKey((k) => k + 1);
      onRead();
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <div className={`app ${openTurn ? "with-doc" : ""}`}>
      <Sidebar
        toast={toast}
        sessions={sessions}
        filter={filter}
        onFilter={setFilter}
        query={query}
        onQuery={setQuery}
        selectedId={selected}
        onSelect={select}
        counts={counts}
        working={working}
        hookLine={hookLineOf(hooks, hookLast)}
        onSettings={() => setSettings(true)}
        phone={phone}
        onPhone={() => setPhoneOpen(true)}
        onNewTask={() => setNewTask(true)}
        onArchive={() => setArchive(true)}
        banner={<UpdateBar state={upd} relayUpgrade={relayUpgrade} progress={updProgress} onChanged={loadUpdate} toast={toast} />}
        onSessionAction={onSessionAction}
        onReadAll={() => readAll(null, "모든 세션").catch((e) => toast(String(e)))}
        onHistoryChat={() => setHistoryChat((v) => !v)}
        historyChatOpen={historyChat}
        sched={schedCount}
        onSched={() => setSchedOpen(true)}
        onDecide={() => setClearDlg({ onlyUndecided: true })}
        searchRef={searchRef}
      />

      {historyChat ? (
        <HistoryChat
          onSettings={() => setSettings(true)}
          toast={toast}
          onOpenTurn={(sid, turnId) => {
            // 이력 보관 세션은 이력 탭에서만 보이므로 목록 필터를 맞춰 준다
            setQuery("");
            setHistoryChat(false);
            setSelected(sid);
            setOpenTurn(turnId);
          }}
        />
      ) : selected ? (
        <ChatView
          key={selected}
          sessionId={selected}
          refreshKey={chatKey}
          openTurnId={openTurn}
          onOpenTurn={setOpenTurn}
          onRead={onRead}
          onReadAll={readAll}
          toast={toast}
          onGone={onGone}
          onManageTags={() => setTagMgr(true)}
        />
      ) : (
        <section className="chat empty">
          <p>{working ? "대화 기록을 모으는 중입니다…" : "왼쪽에서 세션을 고르세요."}</p>
        </section>
      )}

      {openTurn && (
        <DocPanel
          turnId={openTurn}
          refreshKey={docKey}
          onClose={() => setOpenTurn(null)}
          onNavigate={setOpenTurn}
          onChanged={onDocChanged}
          toast={toast}
        />
      )}

      {settings && (
        <Settings
          onClose={() => {
            setSettings(false);
            loadUpdate();
          }}
          onHooksChanged={() => refreshHooks()}
          onTags={() => {
            setSettings(false);
            setTagMgr(true);
          }}
          toast={toast}
        />
      )}
      {tagMgr && <TagManager onClose={() => setTagMgr(false)} toast={toast} onChanged={onDocChanged} />}
      {clearDlg && <ClearDialog onlyUndecided={clearDlg.onlyUndecided} onClose={() => setClearDlg(null)} onChanged={onDocChanged} toast={toast} />}
      {schedOpen && <ScheduleList toast={toast} onClose={() => setSchedOpen(false)} onOpenSession={(sid) => (setQuery(""), setFilter("all"), select(sid))} />}
      {phoneOpen && <PhoneDialog onClose={() => setPhoneOpen(false)} toast={toast} />}
      {archive && (
        <ArchiveView
          toast={toast}
          onClose={() => setArchive(false)}
          onSessionsDeleted={forgetSessions}
          onOpenTurn={(sid, turnId) => {
            setQuery("");
            setFilter("all");
            setSelected(sid);
            setOpenTurn(turnId);
          }}
        />
      )}
      {newTask && (
        <NewTaskDialog
          toast={toast}
          onClose={() => setNewTask(false)}
          onStarted={(short) => {
            setNewTask(false);
            openWhenListed.current = { short, until: Date.now() + 60_000 };
            setQuery("");
            setFilter("all");
            toast("백그라운드에서 시작했습니다 — 곧 목록에 나타납니다");
            loadList();
          }}
        />
      )}
      {pair && <PairDialog name={pair.name} sas={pair.sas} onDone={() => setPair(null)} toast={toast} />}
      {whatsNew && (
        <WhatsNew
          version={whatsNew.version}
          notes={whatsNew.notes}
          onClose={() => {
            api.updateAck();
            setWhatsNew(null);
          }}
        />
      )}
      {toastMsg && (
        <div className="toast" role="status">
          <span>{toastMsg.msg}</span>
          {toastMsg.action && (
            <button
              className="toast-action"
              onClick={() => {
                const a = toastMsg.action!;
                setToastMsg(null);
                Promise.resolve(a.run()).catch((e) => toast(String(e)));
              }}
            >
              {toastMsg.action.label}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
