import { useEffect, useRef, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { Check, Copy } from "lucide-react";
import { api, type About } from "../api";

/** 문제 신고에 붙여 넣을 한 줄 — 버전·빌드 시각(UTC 그대로)·스키마 버전만(경로·계정 등 개인 정보 없음) */
export function aboutReport(a: About): string {
  return `AI Inbox v${a.version} · 빌드 ${a.build_time} UTC · DB 스키마 v${a.schema_version}`;
}

/** 빌드 시각은 UTC "YYYY-MM-DD HH:mm"(build.rs — CI 러너·빌드한 PC 의 시간대와 무관) → 이 PC 의 시간대로. 형식이 다르면 그대로 */
export function localBuild(t: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2})$/.exec(t);
  if (!m) return t;
  const d = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5]));
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** "2026-09-30 14:05"(UTC) → 이 PC 시각의 "09-30 23:05" */
function shortBuild(t: string): string {
  const l = localBuild(t);
  return l.length >= 16 ? l.slice(5, 16) : l;
}

export function useAbout(): About | null {
  const [about, setAbout] = useState<About | null>(null);
  useEffect(() => {
    api.about().then(setAbout).catch(() => {});
  }, []);
  return about;
}

export function CopyReport({ about, toast }: { about: About; toast?: (m: string) => void }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="about-copy"
      title="버전·빌드 시각·DB 스키마 버전만 복사합니다 — 경로·계정 등 개인 정보는 들어 있지 않습니다"
      onClick={async () => {
        try {
          await writeText(aboutReport(about));
          setDone(true);
          window.setTimeout(() => setDone(false), 1800);
          toast?.("신고용 정보를 복사했습니다");
        } catch {
          toast?.("복사하지 못했습니다");
        }
      }}
    >
      {done ? <Check size={13} /> : <Copy size={13} />}
      {done ? "복사됨" : "신고용으로 복사"}
    </button>
  );
}

/** 사이드바 아래에 늘 보이는 `v0.10.0 · 빌드 09-30 14:05` — 누르면 정보 창 */
export function AboutBadge({ toast }: { toast: (m: string) => void }) {
  const about = useAbout();
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key);
    };
  }, [open]);
  if (!about) return null;
  return (
    <div className="about-wrap" ref={wrap}>
      <button className="about-badge" onClick={() => setOpen((o) => !o)} title="AI Inbox 정보 — 버전·빌드 시각" aria-expanded={open}>
        <span className="about-ver">v{about.version}</span>
        <span className="about-build">{`\u00a0·\u00a0빌드\u00a0${shortBuild(about.build_time)}`}</span>
      </button>
      {open && (
        <div className="about-pop" role="dialog" aria-label="AI Inbox 정보">
          <h4>AI Inbox 정보</h4>
          <AboutRows about={about} />
          <CopyReport about={about} toast={toast} />
        </div>
      )}
    </div>
  );
}

export function AboutRows({ about }: { about: About }) {
  return (
    <dl className="about-kv">
      <dt>버전</dt>
      <dd>v{about.version}</dd>
      <dt>빌드 시각</dt>
      <dd>{`${localBuild(about.build_time)} (UTC ${about.build_time})`}</dd>
      <dt>DB 스키마</dt>
      <dd>v{about.schema_version}</dd>
      <dt>데이터 폴더</dt>
      <dd className="about-path" title={about.data_dir}>
        {about.data_dir}
      </dd>
    </dl>
  );
}
