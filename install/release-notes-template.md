<details open>
<summary><b>🇨🇳 简体中文</b></summary>

- 🖱️ **Gambit 输入框右键菜单全区域可用**:此前面板停靠后,输入框只有第一行右键能弹出菜单,其余区域右键无反应。现已修复,在输入框任意位置右键均可弹出菜单。
- 🖥️ **修复侧栏展开/收起时的终端乱码**:展开或收起左右两侧面板时,中间终端曾出现画面错乱/乱码。现已消除——收起动画期间终端保持稳定,动画结束后自动对齐到新尺寸。
- 📋 **修复打开历史记录后复制失效**:此前 v3.2.3 引入的 OSC 52 处理器会在打开历史会话时把内部内容写进系统剪贴板,冲掉用户真正要粘贴的内容。已移除该处理器——拖选复制照常工作且更舒服(#111 回归)。

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

- 🖱️ **Gambit 輸入框右鍵選單全區域可用**:此前面板停靠後,輸入框只有第一行右鍵能彈出選單,其餘區域右鍵無反應。現已修復,在輸入框任意位置右鍵均可彈出選單。
- 🖥️ **修復側欄展開/收起時的終端亂碼**:展開或收起左右兩側面板時,中間終端曾出現畫面錯亂/亂碼。現已消除——收起動畫期間終端保持穩定,動畫結束後自動對齊到新尺寸。
- 📋 **修復打開歷史記錄後複製失效**:此前 v3.2.3 引入的 OSC 52 處理器會在打開歷史工作階段時把內部內容寫進系統剪貼簿,沖掉使用者真正要貼上的內容。已移除該處理器——拖選複製照常工作且更舒服(#111 回歸)。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

- 🖱️ **Gambit input right-click menu works everywhere**: After the panel was docked, only the first line of the input box showed a right-click menu; right-clicking elsewhere did nothing. Fixed — the menu now appears anywhere inside the input box.
- 🖥️ **Fixed terminal garble when collapsing/expanding side panels**: Collapsing or expanding the left/right panels used to garble the center terminal. Eliminated — the terminal stays stable during the slide animation and snaps to the new size once it settles.
- 📋 **Fixed copy breaking after opening history**: The OSC 52 handler introduced in v3.2.3 wrote internal content to the system clipboard when opening chat history, clobbering what you actually wanted to paste. The handler is removed — drag-select copy works as normal and feels better (#111 regression).

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

- 🖱️ **Gambit 入力欄の右クリックメニューが全域で使用可能**:パネル停靠後、入力欄の 1 行目でしか右クリックメニューが出ず、それ以外の領域の右クリックが無反応だった問題を修正。入力欄のどこで右クリックしてもメニューが出るようになりました。
- 🖥️ **サイドパネル開閉時の端末文字化けを修正**:左右パネルの展開/収納時に中央の端末が乱れていた問題を解消。スライドアニメーション中は端末が安定し、終了後に新しいサイズへ自動整列します。
- 📋 **履歴を開いた後のコピー失効を修正**:v3.2.3 で導入した OSC 52 ハンドラが履歴セッションを開く際に内部内容をシステムクリップボードへ書き込み、実際に貼りたかった内容を上書きしていた問題を解消。ハンドラを削除し、ドラッグ選択によるコピーは従来通り快適に動作します(#111 リグレッション)。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

- 🖱️ **Gambit 입력란 우클릭 메뉴 전 영역 사용 가능**:패널 도킹 후 입력란 첫 줄에서만 우클릭 메뉴가 뜨고 나머지 영역은 반응 없던 문제 수정. 입력란 어디서 우클릭해도 메뉴가 나타납니다.
- 🖥️ **측면 패널 펼치기/접기 시 터미널 깨짐 수정**:좌우 패널을 펼치거나 접을 때 중앙 터미널이 깨지던 문제 해결. 슬라이드 애니메이션 중 터미널은 안정을 유지하고, 종료 후 새 크기에 자동 정렬됩니다.
- 📋 **히스토리 열람 후 복사 실패 수정**:v3.2.3 에서 도입한 OSC 52 핸들러가 히스토리 세션을 열 때 내부 내용을 시스템 클립보드에 써, 실제로 붙여넣을 내용을 덮어쓰던 문제 해결. 핸들러를 제거하여 드래그 선택 복사가 종래처럼 쾌적하게 동작합니다(#111 리그레션).

</details>
