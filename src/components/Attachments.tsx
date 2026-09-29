import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronLeft, ChevronRight, FolderOpen, Loader2, Trash2, X } from "lucide-react";
import { api, type AttMeta, type AttSize } from "../api";

/** 한 메시지에 붙일 수 있는 이미지 수 (Rust attach::MAX_PER_MESSAGE 와 같아야 한다) */
export const MAX_ATTS = 5;
const MAX_BYTES = 40 * 1024 * 1024;
const IMAGE_NAME = /\.(png|jpe?g|gif|webp|heic|heif|tiff?|bmp)$/i;

// ── 그림 바이트 → blob URL (같은 썸네일을 여러 곳에서 그리므로 모아 둔다) ──────────
const cache = new Map<string, Promise<string>>();
const order: string[] = [];
const CACHE_MAX = 400;

export function attUrl(id: string, size: AttSize): Promise<string> {
  const key = `${size}:${id}`;
  let p = cache.get(key);
  if (!p) {
    p = api.attachmentGet(id, size).then((buf) => URL.createObjectURL(new Blob([buf])));
    p.catch(() => cache.delete(key));
    cache.set(key, p);
    order.push(key);
    while (order.length > CACHE_MAX) {
      const old = order.shift()!;
      const q = cache.get(old);
      cache.delete(old);
      q?.then((u) => URL.revokeObjectURL(u)).catch(() => {});
    }
  }
  return p;
}

/** 지운 이미지는 모아 둔 URL 도 버린다 */
export function forgetAtt(id: string) {
  for (const size of ["thumb", "view", "orig"] as AttSize[]) {
    const key = `${size}:${id}`;
    cache.get(key)?.then((u) => URL.revokeObjectURL(u)).catch(() => {});
    cache.delete(key);
  }
}

export function useAttUrl(id: string | null, size: AttSize): { url: string | null; failed: boolean } {
  const [state, setState] = useState<{ url: string | null; failed: boolean }>({ url: null, failed: false });
  useEffect(() => {
    let live = true;
    setState({ url: null, failed: false });
    if (!id) return;
    attUrl(id, size)
      .then((url) => live && setState({ url, failed: false }))
      .catch(() => live && setState({ url: null, failed: true }));
    return () => {
      live = false;
    };
  }, [id, size]);
  return state;
}

export function AttThumb({ id, onClick, px = 64, title }: { id: string; onClick?: () => void; px?: number; title?: string }) {
  const { url, failed } = useAttUrl(id, "thumb");
  return (
    <button
      type="button"
      className={`att-thumb ${failed ? "gone" : ""}`}
      style={{ width: px, height: px }}
      title={failed ? "지워진 이미지" : (title ?? "크게 보기")}
      onClick={(e) => {
        e.stopPropagation();
        if (!failed) onClick?.();
      }}
    >
      {url ? <img src={url} alt="" draggable={false} /> : failed ? <span>삭제됨</span> : <Loader2 size={14} className="spin" />}
    </button>
  );
}

export function AttStrip({ ids, onOpen, px }: { ids: string[]; onOpen: (ids: string[], index: number) => void; px?: number }) {
  if (!ids.length) return null;
  return (
    <div className="att-strip">
      {ids.map((id, i) => (
        <AttThumb key={`${id}-${i}`} id={id} px={px} onClick={() => onOpen(ids, i)} />
      ))}
    </div>
  );
}

function sizeText(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  return `${Math.max(1, Math.round(bytes / 1024))}KB`;
}
export { sizeText };

// ── 크게 보기 ─────────────────────────────────────────────────────────────────

interface LightboxProps {
  ids: string[];
  index: number;
  onClose: () => void;
  /** 주면 "지우기" 가 보인다 — 이미지 파일과 모든 메시지의 연결을 지운다 */
  onDelete?: (id: string) => Promise<void>;
  toast: (m: string) => void;
}

export function Lightbox({ ids, index, onClose, onDelete, toast }: LightboxProps) {
  const [i, setI] = useState(Math.min(index, ids.length - 1));
  const [metas, setMetas] = useState<Record<string, AttMeta>>({});
  const [confirm, setConfirm] = useState(false);
  const at = Math.max(0, Math.min(i, ids.length - 1));
  const id = ids[at];
  const { url, failed } = useAttUrl(id ?? null, "orig");

  useEffect(() => {
    api
      .attachmentMeta(ids)
      .then((list) => setMetas(Object.fromEntries(list.map((m) => [m.id, m]))))
      .catch(() => setMetas({}));
  }, [ids]);

  useEffect(() => setConfirm(false), [at, id]);

  const go = useCallback((d: number) => setI((cur) => (cur + d + ids.length) % ids.length), [ids.length]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft" && ids.length > 1) go(-1);
      else if (e.key === "ArrowRight" && ids.length > 1) go(1);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [go, ids.length, onClose]);

  if (!id) return null;
  const m = metas[id];
  return (
    <div className="lightbox" role="dialog" aria-modal="true" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <button className="lb-close icon-btn" title="닫기 (Esc)" onClick={onClose}>
        <X size={20} />
      </button>
      {ids.length > 1 && (
        <button className="lb-nav prev icon-btn" title="앞 이미지 (←)" onClick={() => go(-1)}>
          <ChevronLeft size={26} />
        </button>
      )}
      <figure className="lb-figure" onClick={(e) => e.target === e.currentTarget && onClose()}>
        {url ? (
          <img src={url} alt={m?.name ?? ""} draggable={false} />
        ) : failed ? (
          <p className="lb-gone">이미지 파일이 없습니다 — 지워졌을 수 있습니다.</p>
        ) : (
          <Loader2 size={22} className="spin" />
        )}
      </figure>
      {ids.length > 1 && (
        <button className="lb-nav next icon-btn" title="다음 이미지 (→)" onClick={() => go(1)}>
          <ChevronRight size={26} />
        </button>
      )}
      <footer className="lb-foot">
        <span className="lb-count">
          {at + 1} / {ids.length}
        </span>
        {m && (
          <span className="lb-meta">
            {m.name ? `${m.name} · ` : ""}
            {m.width}×{m.height} · {sizeText(m.bytes)} · {m.source === "phone" ? "폰에서" : "PC 에서"}
          </span>
        )}
        <span className="lb-actions">
          <button
            className="btn"
            onClick={() => api.attachmentReveal(id).catch((e) => toast(String(e)))}
            title="이 이미지 파일이 있는 폴더를 연다"
          >
            <FolderOpen size={14} /> 파일 위치
          </button>
          {onDelete && (
            <button
              className={`btn ${confirm ? "danger" : ""}`}
              onClick={async () => {
                if (!confirm) {
                  setConfirm(true);
                  return;
                }
                try {
                  await onDelete(id);
                  forgetAtt(id);
                  toast("이미지를 지웠습니다");
                  if (ids.length <= 1) onClose();
                } catch (e) {
                  toast(String(e));
                }
              }}
            >
              <Trash2 size={14} /> {confirm ? "정말 지우기" : "지우기"}
            </button>
          )}
        </span>
      </footer>
    </div>
  );
}

// ── 입력창에 붙이기 ───────────────────────────────────────────────────────────

export interface DraftAtt {
  key: string;
  name: string;
  /** 붙이기 전 미리보기(로컬 blob) — 없으면 올린 뒤 썸네일 */
  preview: string | null;
  meta?: AttMeta;
  error?: string;
}

/** 세션을 오가도 붙인 이미지가 남게(앱을 끄면 사라진다) */
const drafts = new Map<string, DraftAtt[]>();
let seq = 0;

export function useAttachDraft(draftKey: string, toast: (m: string) => void) {
  const [items, setItemsRaw] = useState<DraftAtt[]>(() => drafts.get(draftKey) ?? []);
  const ref = useRef(items);
  const setItems = useCallback(
    (f: (prev: DraftAtt[]) => DraftAtt[]) => {
      setItemsRaw((prev) => {
        const next = f(prev);
        ref.current = next;
        if (next.length) drafts.set(draftKey, next);
        else drafts.delete(draftKey);
        return next;
      });
    },
    [draftKey],
  );

  const add = useCallback(
    async (files: File[]) => {
      const room = MAX_ATTS - ref.current.length;
      if (room <= 0) {
        toast(`이미지는 한 번에 ${MAX_ATTS}장까지 붙일 수 있습니다`);
        return;
      }
      const images = files.filter((f) => f.type.startsWith("image/") || IMAGE_NAME.test(f.name));
      if (images.length < files.length) toast("이미지 파일만 붙일 수 있습니다");
      if (images.length > room) toast(`이미지는 한 번에 ${MAX_ATTS}장까지 붙일 수 있습니다 — 앞의 ${room}장만 붙였습니다`);
      for (const file of images.slice(0, room)) {
        if (file.size > MAX_BYTES) {
          toast(`${file.name || "이미지"}: 너무 큽니다(40MB까지)`);
          continue;
        }
        const key = `a${++seq}`;
        const name = file.name || "붙여넣은 이미지.png";
        const preview = URL.createObjectURL(file);
        setItems((prev) => [...prev, { key, name, preview }]);
        try {
          const bytes = new Uint8Array(await file.arrayBuffer());
          const meta = await api.attachmentPut(bytes, name);
          setItems((prev) => (prev.some((x) => x.meta?.id === meta.id && x.key !== key) ? prev.filter((x) => x.key !== key) : prev.map((x) => (x.key === key ? { ...x, meta } : x))));
        } catch (e) {
          toast(`${name}: ${String(e)}`);
          setItems((prev) => prev.map((x) => (x.key === key ? { ...x, error: String(e) } : x)));
        }
      }
    },
    [setItems, toast],
  );

  /** 이미 저장된 이미지를 다시 붙인다(보내지 못한 말 "다시 쓰기") */
  const addSaved = useCallback(
    (metas: AttMeta[]) => {
      setItems((prev) => {
        const have = new Set(prev.map((x) => x.meta?.id));
        const more = metas
          .filter((m) => !have.has(m.id))
          .slice(0, MAX_ATTS - prev.length)
          .map((m) => ({ key: `a${++seq}`, name: m.name ?? "이미지", preview: null, meta: m }));
        return [...prev, ...more];
      });
    },
    [setItems],
  );

  const remove = useCallback(
    (key: string) =>
      setItems((prev) => {
        const x = prev.find((p) => p.key === key);
        if (x?.preview) URL.revokeObjectURL(x.preview);
        return prev.filter((p) => p.key !== key);
      }),
    [setItems],
  );

  const clear = useCallback(
    () =>
      setItems((prev) => {
        prev.forEach((x) => x.preview && URL.revokeObjectURL(x.preview));
        return [];
      }),
    [setItems],
  );

  const ids = items.flatMap((x) => (x.meta ? [x.meta.id] : []));
  const uploading = items.some((x) => !x.meta && !x.error);
  const failed = items.some((x) => x.error);
  return { items, add, addSaved, remove, clear, ids, uploading, failed };
}

export function DraftTray({ items, onRemove }: { items: DraftAtt[]; onRemove: (key: string) => void }) {
  if (!items.length) return null;
  return (
    <div className="draft-tray">
      {items.map((x) => (
        <div key={x.key} className={`draft-att ${x.error ? "bad" : ""}`} title={x.error ?? x.name}>
          {x.preview ? <img src={x.preview} alt="" draggable={false} /> : x.meta ? <DraftThumb id={x.meta.id} /> : null}
          {!x.meta && !x.error && (
            <span className="draft-busy">
              <Loader2 size={14} className="spin" />
            </span>
          )}
          <button type="button" className="draft-x" title="빼기" onClick={() => onRemove(x.key)}>
            <X size={12} />
          </button>
        </div>
      ))}
      <span className="draft-count">
        {items.length} / {MAX_ATTS}
      </span>
    </div>
  );
}

function DraftThumb({ id }: { id: string }) {
  const { url } = useAttUrl(id, "thumb");
  return url ? <img src={url} alt="" draggable={false} /> : null;
}

/** 붙여넣기에서 이미지 파일만(없으면 빈 배열 — 글자 붙여넣기는 그대로 둔다) */
export function imagesFromClipboard(data: DataTransfer | null): File[] {
  if (!data) return [];
  const out: File[] = [];
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind !== "file") continue;
    const f = item.getAsFile();
    if (f && (f.type.startsWith("image/") || IMAGE_NAME.test(f.name))) out.push(f);
  }
  if (!out.length) {
    for (const f of Array.from(data.files ?? [])) {
      if (f.type.startsWith("image/") || IMAGE_NAME.test(f.name)) out.push(f);
    }
  }
  return out;
}

export function hasFiles(e: React.DragEvent): boolean {
  return Array.from(e.dataTransfer?.types ?? []).includes("Files");
}
