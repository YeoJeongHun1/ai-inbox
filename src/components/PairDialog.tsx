import { useEffect, useState } from "react";
import { Smartphone } from "lucide-react";
import { api } from "../api";

/** 폰이 연결을 청했을 때 — 이 PC 에서 허용해야만 연결된다(2분 안에). */
export function PairDialog({ name, sas, onDone, toast }: { name: string; sas: string; onDone: () => void; toast: (m: string) => void }) {
  const [canReply, setCanReply] = useState(true);
  const [left, setLeft] = useState(120);
  useEffect(() => {
    const t = window.setInterval(() => setLeft((s) => s - 1), 1000);
    return () => window.clearInterval(t);
  }, []);
  useEffect(() => {
    if (left <= 0) onDone();
  }, [left, onDone]);

  const decide = async (approve: boolean) => {
    try {
      await api.relayDecidePair(approve, canReply, sas);
      toast(approve ? `${name} 을(를) 연결했습니다` : "연결 요청을 거절했습니다");
    } catch (e) {
      toast(String(e));
    }
    onDone();
  };

  return (
    <div className="modal-back" role="dialog" aria-modal="true">
      <div className="modal pair-dialog">
        <h3>
          <Smartphone size={18} /> 새 기기 연결 요청
        </h3>
        <p className="pair-name">{name}</p>
        <div className="pair-sas">
          <span className="sas-label">확인 코드</span>
          <span className="sas-code">{sas}</span>
        </div>
        <p className="set-note">
          폰 화면의 확인 코드가 이 숫자와 <strong>같을 때만</strong> 허용하세요. 다르면 다른 기기가 끼어든 것이니 거절하세요.
          허용하면 이 폰에서 이 PC 의 세션·요청·응답을 볼 수 있습니다.
        </p>
        <label className="check">
          <input type="checkbox" checked={canReply} onChange={(e) => setCanReply(e.target.checked)} />이 폰에서 세션에 답
          보내기 허용 (나중에 설정에서 바꿀 수 있음)
        </label>
        <div className="set-row">
          <button className="btn primary" onClick={() => decide(true)}>
            허용
          </button>
          <button className="btn" onClick={() => decide(false)}>
            거절
          </button>
          <span className="set-note small">{left}초 뒤 자동으로 거절</span>
        </div>
      </div>
    </div>
  );
}
