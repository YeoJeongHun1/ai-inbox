import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { listen } from "@tauri-apps/api/event";
import { ArrowUp, Clock, ImagePlus, Lock, Reply, X } from "lucide-react";
import { api, quoteLabel, type AttMeta, type QuoteTarget, type SessionHeader, type SendMode } from "../api";
import { DraftTray, MAX_ATTS, hasFiles, imagesFromClipboard, useAttachDraft } from "./Attachments";
import { tagColor, useTags } from "../tags";
import { ScheduleDialog } from "./ScheduleDialog";
import { ScheduleList } from "./ScheduleList";

/** 캐럿 바로 앞의 `#낱말` — 공백·여는 괄호·따옴표 뒤에서 시작한 것만(URL 조각·마크다운 제목은 제외) */
const HASH_AT_CARET = /(?:^|[\s(\[{"'「『“‘,;:])#([\p{L}\p{N}_\-·]*)$/u;

/** 세션을 오가도 쓰던 글이 남게(앱을 끄면 사라진다 — 디스크에 두지 않는다) */
const drafts = new Map<string, string>();

/** Codex 세션: 열려 있으면 Codex 대기열(열린 화면이 바로 가져간다), 꺼져 있으면 codex exec 로 이어서 */
const CODEX_HINT: Partial<Record<SendMode, string>> = {
  live: "열려 있는 Codex 세션에 바로 들어갑니다",
  queue: "Codex 가 일하는 중입니다 — 지금 하던 일이 끝나면 이어서 보냅니다",
  resume: "Codex 세션이 꺼져 있습니다 — 보내면 백그라운드에서 이어서 실행합니다(승인은 묻지 않음 · 전권이던 세션은 폴더 안 쓰기로 낮춤)",
};

const HINT: Record<SendMode, string> = {
  live: "실행 중인 세션에 바로 들어갑니다 — 일하는 중이면 하던 일 사이에 읽습니다",
  queue: "세션이 일하는 중입니다 — 지금 하던 일이 끝나면 이어서 보냅니다",
  approve: "세션이 권한 승인을 기다립니다 — 승인한 뒤 하던 일이 끝나면 이어서 보냅니다",
  connecting: "세션에 연결하는 중입니다 — 보내면 연결되는 대로 들어갑니다",
  resume: "세션이 꺼져 있습니다 — 보내면 백그라운드에서 이어서 실행합니다",
  terminal: "",
};

interface Props {
  session: SessionHeader;
  /** 되돌려 받은 글·이미지(보내지 못한 말 "다시 쓰기") — n 이 바뀔 때마다 입력창에 채운다 */
  seed: { text: string; atts: AttMeta[]; n: number; prepend?: boolean } | null;
  onSent: () => void;
  toast: (m: string) => void;
  /** 답장 대상(요청·결과에 달린 "답장"으로 고른 것) — 보내면 비운다 */
  quote: QuoteTarget | null;
  onClearQuote: () => void;
  onJump: (seq: number) => void;
}

export function Composer({ session, seed, onSent, toast, quote, onClearQuote, onJump }: Props) {
  const sid = session.id;
  const [text, setText] = useState(() => drafts.get(sid) ?? "");
  const [sending, setSending] = useState(false);
  const [dragging, setDragging] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const att = useAttachDraft(sid, toast);
  const { ov } = useTags();
  const [caret, setCaret] = useState(0);
  const [pick, setPick] = useState(0);
  const [tagOff, setTagOff] = useState(false);
  /** 예약 전송: 입력창의 글을 정한 시각에 보내기 · 이 세션의 예약 목록 */
  const [schedDlg, setSchedDlg] = useState(false);
  const [schedList, setSchedList] = useState(false);
  const [schedN, setSchedN] = useState(0);
  const [schedHeld, setSchedHeld] = useState(0);
  useEffect(() => {
    const load = () =>
      api
        .schedList(sid)
        .then((l) => {
          setSchedN(l.filter((i) => i.state === "active").length);
          setSchedHeld(l.filter((i) => i.run?.state === "held").length);
        })
        .catch(() => {});
    load();
    const un = listen("sched-changed", load);
    return () => {
      un.then((f) => f());
    };
  }, [sid]);

  // `#` 뒤에 글자를 치면 이미 있는 태그를 제안한다(공백이 든 이름은 #태그 로 쓸 수 없어 뺀다)
  const hashQuery = tagOff ? null : HASH_AT_CARET.exec(text.slice(0, caret))?.[1] ?? null;
  const suggestions =
    hashQuery === null
      ? []
      : (ov?.tags ?? [])
          .filter((t) => !/\s/.test(t.name) && t.name.toLowerCase().includes(hashQuery.toLowerCase()) && t.name.toLowerCase() !== hashQuery.toLowerCase())
          .sort((a, b) => Number(b.name.toLowerCase().startsWith(hashQuery.toLowerCase())) - Number(a.name.toLowerCase().startsWith(hashQuery.toLowerCase())) || b.turns - a.turns)
          .slice(0, 6);
  const applyTag = (name: string) => {
    const before = text.slice(0, caret);
    const m = HASH_AT_CARET.exec(before);
    if (!m) return;
    const start = caret - m[1].length; // '#' 바로 뒤
    const next = text.slice(0, start) + name + " " + text.slice(caret);
    setText(next);
    const pos = start + name.length + 1;
    requestAnimationFrame(() => {
      ref.current?.setSelectionRange(pos, pos);
      setCaret(pos);
    });
  };

  useEffect(() => {
    if (seed) {
      setText((cur) => (seed.prepend ? seed.text + cur : seed.text));
      if (seed.atts.length) att.addSaved(seed.atts);
      ref.current?.focus();
    }
  }, [seed]); // eslint-disable-line react-hooks/exhaustive-deps

  // 답장을 고르면 바로 쓸 수 있게
  useEffect(() => {
    if (quote) ref.current?.focus();
  }, [quote]);

  useEffect(() => {
    if (text) drafts.set(sid, text);
    else drafts.delete(sid);
  }, [sid, text]);

  // 줄 수만큼 늘어나되 8줄 남짓에서 멈춘다
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [text]);

  if (session.send_mode === "terminal") {
    return (
      <div className="composer blocked">
        <Lock size={15} />
        <p>
          실행 중인 세션에 말을 넣으려면 훅을 업데이트해야 합니다. 세션을 다시 시작할 필요는 없습니다.{" "}
          <button
            className="more"
            onClick={async () => {
              try {
                await api.installHooks();
                toast("훅을 업데이트했습니다 — 몇 초 안에 실행 중인 세션들과 연결됩니다");
                onSent();
              } catch (e) {
                toast(String(e));
              }
            }}
          >
            훅 업데이트
          </button>
        </p>
      </div>
    );
  }

  const canSend = (text.trim().length > 0 || att.ids.length > 0) && !att.uploading && !att.failed && !sending;

  const send = async () => {
    const body = text.trim();
    if (!canSend) {
      if (att.uploading) toast("이미지를 올리는 중입니다 — 잠시 뒤 보내세요");
      else if (att.failed) toast("올리지 못한 이미지가 있습니다 — 빼고 보내세요");
      return;
    }
    setSending(true);
    try {
      await api.sendMessage(sid, body, att.ids, quote);
      setText("");
      att.clear();
      onClearQuote();
      onSent();
    } catch (e) {
      toast(String(e));
    } finally {
      setSending(false);
      ref.current?.focus();
    }
  };

  return (
    <div
      className={`composer ${dragging ? "dragging" : ""}`}
      onDragOver={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={(e) => {
        setDragging(false);
        const files = imagesFromClipboard(e.dataTransfer);
        if (!hasFiles(e)) return;
        e.preventDefault();
        if (files.length) att.add(files);
        else toast("이미지 파일만 붙일 수 있습니다");
      }}
    >
      {(schedN > 0 || schedHeld > 0) && (
        <div className={`sched-chip-row ${schedHeld > 0 ? "warn" : ""}`}>
          <Clock size={13} />
          <button className="more" onClick={() => setSchedList(true)}>
            {schedHeld > 0 ? `예약 ${schedHeld}건이 전달되지 못했습니다 — 처리하기` : `예약 ${schedN}건 대기 중`}
          </button>
        </div>
      )}
      {quote && (
        <div className="quote-bar">
          <Reply size={14} />
          <button className="q-main" title="답장할 요청으로 가기" onClick={() => onJump(quote.seq)}>
            <span className="q-label">{quoteLabel(quote)}에 답장</span>
            <span className="q-text">{quote.text}</span>
          </button>
          <button className="icon-btn" title="답장 취소(Esc)" onClick={onClearQuote}>
            <X size={14} />
          </button>
        </div>
      )}
      <DraftTray items={att.items} onRemove={att.remove} />
      <div className="composer-box">
        {suggestions.length > 0 && (
          <ul className="tag-suggest" role="listbox" aria-label="태그 제안">
            {suggestions.map((t, i) => (
              <li
                key={t.id}
                role="option"
                aria-selected={i === pick}
                className={i === pick ? "on" : ""}
                onMouseDown={(e) => {
                  e.preventDefault(); // 입력창의 포커스를 잃지 않게
                  applyTag(t.name);
                }}
              >
                <span className="ts-dot" style={{ background: tagColor(t) }} />
                #{t.name}
                <span className="ts-n">{t.turns}</span>
              </li>
            ))}
          </ul>
        )}
        <button
          type="button"
          className="attach-btn"
          title={`이미지 붙이기 — 붙여넣기(⌘V)·끌어다 놓기도 됩니다 (${MAX_ATTS}장까지)`}
          disabled={att.items.length >= MAX_ATTS}
          onClick={() => picker.current?.click()}
        >
          <ImagePlus size={17} />
        </button>
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
        <textarea
          ref={ref}
          rows={1}
          value={text}
          maxLength={4000}
          placeholder={att.items.length ? "이미지와 함께 보낼 말 (비워 둬도 됩니다)" : "이 세션에 이어서 시킬 일"}
          spellCheck={false}
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart ?? e.target.value.length);
            setPick(0);
            setTagOff(false);
          }}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
          onPaste={(e) => {
            const files = imagesFromClipboard(e.clipboardData);
            if (!files.length) return; // 글자 붙여넣기는 그대로
            e.preventDefault();
            att.add(files);
          }}
          onKeyDown={(e) => {
            // 한글 조합 중의 Enter 는 글자 확정이다 — 보내지 않는다
            if (e.nativeEvent.isComposing || e.keyCode === 229) return;
            if (suggestions.length > 0) {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                setPick((p) => (p + (e.key === "ArrowDown" ? 1 : suggestions.length - 1)) % suggestions.length);
                return;
              }
              if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
                e.preventDefault();
                applyTag(suggestions[Math.min(pick, suggestions.length - 1)].name);
                return;
              }
              if (e.key === "Escape") {
                e.preventDefault();
                setTagOff(true);
                return;
              }
            }
            if (e.key === "Escape" && quote) {
              e.preventDefault();
              onClearQuote();
              return;
            }
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        <button
          type="button"
          className="attach-btn"
          title="예약해서 보내기 — 정한 시각에 이 세션에 넣습니다(AI Inbox 가 켜져 있을 때만)"
          disabled={!canSend}
          onClick={() => setSchedDlg(true)}
        >
          <Clock size={17} />
        </button>
        <button className="send-btn" title="보내기 (Enter · 줄바꿈은 Shift+Enter)" disabled={!canSend} onClick={send}>
          <ArrowUp size={17} />
        </button>
      </div>
      {schedDlg && (
        <ScheduleDialog
          sessionId={sid}
          sessionName={session.name}
          text={text}
          attIds={att.ids}
          quote={quote}
          toast={toast}
          onClose={() => setSchedDlg(false)}
          onDone={() => {
            setSchedDlg(false);
            setText("");
            att.clear();
            onClearQuote();
          }}
        />
      )}
      {schedList && <ScheduleList sessionId={sid} toast={toast} onClose={() => setSchedList(false)} onOpenSession={() => {}} />}
      <div className="composer-hint">
        <span>{(session.agent === "codex" && CODEX_HINT[session.send_mode]) || HINT[session.send_mode]} · #태그 를 쓰면 이 요청에 그 태그가 붙습니다</span>
        {session.attach_command && (session.send_mode === "approve" || session.send_mode === "queue") && (
          <button
            className="hint-cmd"
            title="터미널에서 이 백그라운드 세션을 여는 명령을 복사"
            onClick={async () => {
              await writeText(session.attach_command!);
              toast("명령을 복사했습니다 — 터미널에 붙여 넣으세요");
            }}
          >
            {session.attach_command}
          </button>
        )}
      </div>
    </div>
  );
}
