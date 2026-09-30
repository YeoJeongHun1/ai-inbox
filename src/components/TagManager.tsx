import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, FolderSearch, Trash2, X } from "lucide-react";
import { api, type FolderSuggestion, type TagAiPending, type TagAiStatus, type TagInfo } from "../api";
import { PALETTE, reloadTags, tagColor, useTags } from "../tags";

interface Props {
  onClose: () => void;
  toast: (m: string) => void;
  /** 태그·규칙이 바뀌었다 — 열려 있는 대화·목록을 다시 읽는다 */
  onChanged: () => void;
}

/** 태그 관리 — 이름·색·병합·삭제, 자동 규칙(경로·낱말), 폴더로 태그 만들기, 모델 제안(선택) */
export function TagManager({ onClose, toast, onChanged }: Props) {
  const { ov } = useTags();
  const [open, setOpen] = useState<number | null>(null);
  const [name, setName] = useState("");
  const [folders, setFolders] = useState<FolderSuggestion[] | null>(null);
  const [scanning, setScanning] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const done = useCallback(() => {
    reloadTags(true);
    onChanged();
  }, [onChanged]);
  const run = async (f: () => Promise<unknown>, ok?: string) => {
    try {
      await f();
      if (ok) toast(ok);
      done();
    } catch (e) {
      toast(String(e));
    }
  };

  const scan = async () => {
    setScanning(true);
    try {
      setFolders(await api.tagSuggestFolders());
    } catch (e) {
      toast(String(e));
    } finally {
      setScanning(false);
    }
  };
  const fromFolder = (f: FolderSuggestion) =>
    run(async () => {
      const existing = ov?.tags.find((t) => t.name.toLowerCase() === f.name.toLowerCase());
      const id = existing ? existing.id : await api.tagCreate(f.name);
      await api.tagRuleAdd(id, "path", f.pattern, true);
      setFolders((cur) => (cur ? cur.filter((x) => x.pattern !== f.pattern) : cur));
    }, `「${f.name}」 태그와 규칙(${f.pattern})을 만들었습니다 — 앞으로의 요청부터 붙습니다`);

  const tags = ov?.tags ?? [];
  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal tag-mgr" role="dialog" aria-label="태그 관리">
        <header className="modal-head">
          <h2>요청 태그</h2>
          <button className="icon-btn" onClick={onClose} title="닫기 (Esc)">
            <X size={17} />
          </button>
        </header>

        <section className="set">
          <p className="set-note">
            한 세션에서 여러 주제를 다룰 때 요청마다 태그를 달아 나눠 봅니다. 태그는 <strong>요청을 보내는 순간</strong> 정해집니다 — 글에 직접 쓴 <code>#태그</code>, 없으면 이 PC 의 규칙(낱말 · 작업 폴더), 그것도 없으면 작업 중인 저장소 폴더 이름으로 프로젝트 태그를 자동으로 만듭니다.
            바깥으로 아무것도 보내지 않습니다. 이 기능 전의 지난 요청은 <strong>미분류</strong>로 두며(직접 붙이면 됩니다), 직접 붙이거나 뗀 태그는 자동이 바꾸지 않습니다. 규칙을 바꿔도 앞으로의 요청부터 적용됩니다.
          </p>
          <form
            className="set-line tm-new"
            onSubmit={(e) => {
              e.preventDefault();
              const n = name.trim();
              if (!n) return;
              run(() => api.tagCreate(n), `「${n}」 태그를 만들었습니다`).then(() => setName(""));
            }}
          >
            <input value={name} maxLength={24} placeholder="새 태그 이름" spellCheck={false} onChange={(e) => setName(e.target.value)} />
            <button className="btn" type="submit" disabled={!name.trim()}>
              만들기
            </button>
          </form>
          {ov && (
            <p className="set-note small">
              전체 요청 {ov.total} · 태그 없는 요청 {ov.untagged}
            </p>
          )}
        </section>

        <section className="set">
          <h3>태그 {tags.length}</h3>
          {tags.length === 0 && <p className="set-note">태그가 없습니다. 위에서 만들거나 아래 "작업 폴더로 만들기"를 써 보세요.</p>}
          <ul className="tm-list">
            {tags.map((t) => (
              <TagRow key={t.id} t={t} others={tags.filter((x) => x.id !== t.id)} open={open === t.id} onToggle={() => setOpen(open === t.id ? null : t.id)} run={run} />
            ))}
          </ul>
          <div className="set-row">
            <button className="btn" onClick={() => run(() => api.tagResetDefaults().then((n) => toast(n ? `기본 태그 ${n}개를 되살렸습니다` : "되살릴 기본 태그가 없습니다")))} title="처음에 들어 있던 '종류' 태그(버그·배포·문서 등) 중 지운 것을 다시 만듭니다">
              기본 태그 되살리기
            </button>
          </div>
        </section>

        <section className="set">
          <h3>작업 폴더로 만들기</h3>
          <p className="set-note">
            이 PC 의 요청들이 자주 다룬 폴더를 훑어, 폴더 이름 그대로 태그 + 자동 규칙을 만들자고 제안합니다(이름은 앱에 내장된 것이 없고 이 PC 의 기록에서만 나옵니다).
          </p>
          <div className="set-row">
            <button className="btn" onClick={scan} disabled={scanning}>
              <FolderSearch size={14} /> {scanning ? "훑는 중…" : folders ? "다시 훑기" : "작업 폴더 훑기"}
            </button>
          </div>
          {folders && folders.length === 0 && <p className="set-note small">제안할 폴더가 없습니다(이미 규칙이 있거나 요청이 적습니다).</p>}
          {folders && folders.length > 0 && (
            <ul className="tm-folders">
              {folders.map((f) => (
                <li key={f.path} className={f.depth ? "sub" : ""}>
                  <span className="tm-fname">{f.name}</span>
                  <span className="tm-fpath" title={f.path}>
                    {f.path}
                  </span>
                  <span className="tm-fn">{f.turns}건</span>
                  <button className="btn" onClick={() => fromFolder(f)}>
                    태그로 만들기
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <AiSection toast={toast} tags={tags} onChanged={done} />
      </div>
    </div>
  );
}

function TagRow({
  t,
  others,
  open,
  onToggle,
  run,
}: {
  t: TagInfo;
  others: TagInfo[];
  open: boolean;
  onToggle: () => void;
  run: (f: () => Promise<unknown>, ok?: string) => Promise<void>;
}) {
  const [name, setName] = useState(t.name);
  const [colors, setColors] = useState(false);
  const [kind, setKind] = useState<"keyword" | "path">("keyword");
  const [pattern, setPattern] = useState("");
  useEffect(() => setName(t.name), [t.name]);
  const rename = () => {
    const n = name.trim();
    if (!n || n === t.name) return setName(t.name);
    run(() => api.tagUpdate(t.id, { name: n }), "이름을 바꿨습니다").catch(() => setName(t.name));
  };
  return (
    <li className="tm-row">
      <div className="tm-line">
        <button className="icon-btn" onClick={onToggle} title="자동 규칙 보기·고치기" aria-expanded={open}>
          {open ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
        </button>
        <button className="tm-swatch" style={{ background: tagColor(t) }} title="색 고르기" onClick={() => setColors(!colors)} />
        <input
          className="tm-name"
          value={name}
          maxLength={24}
          spellCheck={false}
          onChange={(e) => setName(e.target.value)}
          onBlur={rename}
          onKeyDown={(e) => e.key === "Enter" && (e.currentTarget.blur(), undefined)}
        />
        <span className="tm-n" title="이 태그가 붙은 요청 수">
          {t.turns}
        </span>
        {t.rules.some((r) => r.source === "hook") && (
          <span className="tm-auto" title="요청을 보낼 때 작업 중인 저장소 폴더 이름으로 자동 만든 프로젝트 태그 — 이름 바꾸기·색·병합·삭제가 됩니다(지우면 다시 만들지 않습니다)">
            자동 생성
          </span>
        )}
        <label className="tm-minor" title="작은 태그 — 종류 표식으로, 사이드바의 대표 태그에서 뒤로 밀립니다">
          <input type="checkbox" checked={t.minor} onChange={(e) => run(() => api.tagUpdate(t.id, { minor: e.target.checked }))} /> 작은 태그
        </label>
        <select
          className="tm-merge"
          value=""
          onChange={(e) => {
            const to = Number(e.target.value);
            const target = others.find((o) => o.id === to);
            if (!target) return;
            if (window.confirm(`「${t.name}」 을(를) 「${target.name}」 으로 합칩니다. 요청 표식과 규칙이 옮겨지고 「${t.name}」 은 사라집니다.`)) {
              run(() => api.tagMerge(t.id, to), `「${target.name}」 으로 합쳤습니다`);
            }
          }}
          aria-label="다른 태그로 병합"
        >
          <option value="">병합…</option>
          {others.map((o) => (
            <option key={o.id} value={o.id}>
              → {o.name}
            </option>
          ))}
        </select>
        <button
          className="icon-btn"
          title="태그 삭제 — 요청에서 이 태그가 빠집니다(요청은 그대로)"
          onClick={() => window.confirm(`「${t.name}」 태그를 지웁니다. ${t.turns}개 요청에서 이 태그가 빠집니다.`) && run(() => api.tagDelete(t.id), "지웠습니다")}
        >
          <Trash2 size={15} />
        </button>
      </div>
      {t.promote && (
        <div className="tm-promote">
          <span>
            <code>#{t.promote}</code> 로 {t.hashtag_uses}번 지목했습니다. 글에 <code>{t.promote}</code> 낱말만 써도 붙도록 규칙으로 올릴까요?
          </span>
          <button className="btn" onClick={() => run(() => api.tagRuleAdd(t.id, "keyword", t.promote!, true), `「${t.promote}」 낱말 규칙을 추가했습니다 — 앞으로의 요청부터 적용됩니다`)}>
            낱말 규칙으로
          </button>
        </div>
      )}
      {colors && (
        <div className="tm-colors">
          {PALETTE.map((c) => (
            <button key={c} className={`tm-swatch ${c === t.color ? "on" : ""}`} style={{ background: c }} onClick={() => run(() => api.tagUpdate(t.id, { color: c })).then(() => setColors(false))} aria-label={c} />
          ))}
        </div>
      )}
      {open && (
        <div className="tm-rules">
          {t.rules.length === 0 && <p className="set-note small">규칙이 없습니다 — 이 태그는 직접 붙일 때만 달립니다.</p>}
          {t.rules.map((r) => (
            <div key={r.id} className="tm-rule">
              <span className="tm-kind">{r.kind === "path" ? "경로" : "낱말"}</span>
              <code>{r.pattern}</code>
              <span className="tm-src">{r.source === "default" ? "기본" : r.source === "suggested" ? "제안 수락" : r.source === "hook" ? "자동 생성(저장소 폴더)" : "직접"}</span>
              <button className="icon-btn" title="규칙 삭제" onClick={() => run(() => api.tagRuleRemove(r.id))}>
                <X size={13} />
              </button>
            </div>
          ))}
          <form
            className="tm-add"
            onSubmit={(e) => {
              e.preventDefault();
              const p = pattern.trim();
              if (!p) return;
              run(() => api.tagRuleAdd(t.id, kind, p), "규칙을 추가했습니다 — 앞으로의 요청부터 적용됩니다").then(() => setPattern(""));
            }}
          >
            <select value={kind} onChange={(e) => setKind(e.target.value as "keyword" | "path")}>
              <option value="keyword">낱말 — 요청·응답 글에 있으면</option>
              <option value="path">경로 — 작업 폴더 경로에 있으면</option>
            </select>
            <input value={pattern} maxLength={120} placeholder={kind === "path" ? "예: /폴더이름/" : "예: 낱말"} spellCheck={false} onChange={(e) => setPattern(e.target.value)} />
            <button className="btn" type="submit" disabled={!pattern.trim()}>
              추가
            </button>
          </form>
          <p className="set-note small">
            낱말은 대소문자를 가리지 않고, 영문·숫자 낱말은 단어 단위로만 맞습니다(<code>api</code> 는 <code>capital</code> 에 걸리지 않음). 경로는 조각이 들어 있으면 맞습니다. 요청 글에 낱말이 있으면 그 태그가(종류 태그 외에 큰 태그가 있으면 폴더는 보지 않음), 없으면 작업 폴더의 경로 규칙이 맞는 태그가 붙습니다. 24자 이하의 짧은 말은 직전 요청의 태그를 이어받습니다.
          </p>
        </div>
      )}
    </li>
  );
}

/** 모델 제안(선택 · 기본 꺼짐) — 켜고 외부 전송에 동의해야만, 누를 때만 동작한다 */
function AiSection({ toast, tags, onChanged }: { toast: (m: string) => void; tags: TagInfo[]; onChanged: () => void }) {
  const [st, setSt] = useState<TagAiStatus | null>(null);
  const [pending, setPending] = useState<TagAiPending[]>([]);
  const [busy, setBusy] = useState(false);
  const load = useCallback(() => {
    api.tagAiStatus().then(setSt).catch(() => setSt(null));
    api.tagAiPending(80).then(setPending).catch(() => setPending([]));
  }, []);
  useEffect(load, [load]);
  // 구독 CLI 감지(처음 한 번 — 결과는 앱이 기억한다)
  useEffect(() => {
    api.historyDetect().then(() => api.tagAiStatus().then(setSt)).catch(() => {});
  }, []);
  if (!st) return null;
  const ready = st.enabled && st.consent && st.active !== null;
  const set = async (k: "enabled" | "consent", v: boolean) => {
    try {
      await api.tagAiSet(k, v ? "1" : "0");
      load();
    } catch (e) {
      toast(String(e));
    }
  };
  const ask = async () => {
    setBusy(true);
    try {
      const r = await api.tagAiSuggest();
      toast(r.asked ? `${r.asked}개 요청을 물어봐 ${r.suggested}개를 제안받았습니다 — 아래에서 받아들이세요` : "물어볼 미분류 요청이 없습니다");
      load();
      onChanged();
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  const decide = async (p: TagAiPending, accept: boolean) => {
    await api.tagAiDecide(p.turn_id, p.tag_id, accept).catch((e) => toast(String(e)));
    load();
    onChanged();
  };
  const nameOf = (id: number) => tags.find((t) => t.id === id)?.name ?? "?";
  return (
    <section className="set">
      <h3>미분류 요청 모델로 태깅 제안 (선택)</h3>
      <p className="set-note">
        규칙으로 못 붙인 요청을 이 컴퓨터의 <strong>구독 CLI(Claude Code 또는 Codex{st.model ? ` · ${st.model}` : ""})</strong>에 물어 태그를 제안받습니다. 기본은 꺼져 있고, 이것 없이도 모든 기능이 동작합니다. 켜고 동의한 뒤 아래 버튼을 눌렀을 때만, 요청 <strong>첫 줄 300자와 작업 폴더 이름</strong>(비밀값은 가림)과 태그 이름 목록이 그 구독 서비스의 서버로 나가고 구독 사용량이 소모됩니다. 결과 글·전체 대화는 보내지 않습니다.
        제안은 받아들이기 전에는 표식이 아닙니다. 서비스 선택·모델·하루 호출 상한은 "대화 이력 검색"과 같습니다({st.calls_today}/{st.daily_cap}).
      </p>
      <label className="set-line">
        <input type="checkbox" checked={st.enabled} onChange={(e) => set("enabled", e.target.checked)} /> <span>이 기능 켜기</span>
      </label>
      <label className="set-line">
        <input type="checkbox" checked={st.consent} onChange={(e) => set("consent", e.target.checked)} /> <span>위 내용이 구독 서비스(Anthropic 또는 OpenAI)의 서버로 전송되고 구독 사용량이 소모되는 것에 동의합니다</span>
      </label>
      {st.active === null && <p className="set-note small">쓸 수 있는 구독 CLI 를 아직 찾지 못했습니다 — 설정의 "대화 이력 검색"에서 감지 상태를 확인하세요(Claude Code 또는 Codex 설치·로그인).</p>}
      <div className="set-row">
        <button className="btn" disabled={!ready || busy || tags.length === 0} onClick={ask}>
          {busy ? "묻는 중…" : "미분류 요청 태깅 제안 받기"}
        </button>
        {pending.length > 0 && (
          <>
            <button className="btn" onClick={() => api.tagAiDecideAll(true).then((n) => { toast(`${n}개를 받아들였습니다`); load(); onChanged(); })}>
              제안 모두 받아들이기
            </button>
            <button className="btn" onClick={() => api.tagAiDecideAll(false).then(() => { load(); onChanged(); })}>
              모두 물리치기
            </button>
          </>
        )}
      </div>
      {pending.length > 0 && (
        <ul className="tm-ai">
          {pending.map((p) => (
            <li key={`${p.turn_id}-${p.tag_id}`}>
              <span className="tm-ai-t" title={p.prompt}>
                <em>{p.session_name} #{p.seq}</em> {p.prompt}
              </span>
              <span className="tag-badge ai">{nameOf(p.tag_id)}</span>
              <button className="btn" onClick={() => decide(p, true)}>받아들이기</button>
              <button className="btn" onClick={() => decide(p, false)}>물리치기</button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
