import { useEffect } from "react";
import { X } from "lucide-react";
import { PhoneSection } from "./Settings";

/** 메인 화면의 "폰 연결" 버튼 — 설정 안쪽까지 가지 않고 바로 QR·연결된 폰을 본다 */
export function PhoneDialog({ onClose, toast }: { onClose: () => void; toast: (m: string) => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="modal-back" role="dialog" aria-modal="true" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <header className="modal-head">
          <h2>폰 연결</h2>
          <button className="icon-btn" title="닫기" onClick={onClose}>
            <X size={18} />
          </button>
        </header>
        <PhoneSection toast={toast} autoOffer />
      </div>
    </div>
  );
}
