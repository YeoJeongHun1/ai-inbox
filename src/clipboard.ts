/** 클립보드에 넣는다. 실패해도 던지지 않고 false — 다른 프로그램이 클립보드를 잡고 있으면(Windows 에서 실측) 쓰기가 실패하는데,
 *  그때 아무 안내도 없으면 사용자는 복사된 줄 알고 붙여 넣는다. 부르는 쪽이 글을 직접 보여 준다(`CopyFallback`). */
export async function tryCopy(text: string, write: (text: string) => Promise<void>): Promise<boolean> {
  try {
    await write(text);
    return true;
  } catch {
    return false;
  }
}
