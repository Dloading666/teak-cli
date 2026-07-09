<details open>
<summary><b>🇨🇳 简体中文</b></summary>

- 修复:用 winget / Microsoft Store 装的 PowerShell 7(MSIX 包)现在能被识别了。之前 设置 → 终端 里根本不显示 PowerShell 7 选项——它的真程序锁在 WindowsApps 里、PATH 上只留一个空壳别名,旧逻辑把空壳当成"没装"过滤掉了。现在改成直接探安装位置(参考 VS Code PowerShell 扩展的做法),装了就显示、带精确版本号、能正常启动。从 7.7 起微软只发 MSIX 包,这个分支必须覆盖。
- 优化:修改记录里点文件看比对、放大成中间 tab 时,背景现在和中间区域完全统一(之前 tab 和比对内容之间有一道色阶,不够连贯)。
- 优化:比对里折叠的"⋯ N 行未改动"提示去掉了斜体、换成界面统一字体——之前是等宽斜体,中文等小语种发虚看不清,现在高清易读。

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

- 修復:用 winget / Microsoft Store 安裝的 PowerShell 7(MSIX 包)現在能被識別了。之前 設定 → 終端 裡根本不顯示 PowerShell 7 選項——它的真程式鎖在 WindowsApps 裡、PATH 上只留一個空殼別名,舊邏輯把空殼當成「沒裝」過濾掉了。現在改成直接探安裝位置(參考 VS Code PowerShell 擴充的做法),裝了就顯示、帶精確版本號、能正常啟動。從 7.7 起微軟只發 MSIX 包,這個分支必須覆蓋。
- 優化:修改記錄裡點檔案看比對、放大成中間 tab 時,背景現在和中間區域完全統一(之前 tab 和比對內容之間有一道色階,不夠連貫)。
- 優化:比對裡摺疊的「⋯ N 行未變更」提示去掉了斜體、換成介面統一字型——之前是等寬斜體,中文等小語種發虛看不清,現在高清易讀。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

- Fixed: PowerShell 7 installed via winget / Microsoft Store (the MSIX package) is now detected. Settings → Terminal previously didn't show a PowerShell 7 option at all — the real binary is locked under WindowsApps and PATH only carries a hollow App Execution Alias, which the old probe filtered out as "not installed". Detection now probes install locations directly (mirroring the VS Code PowerShell extension): if it's installed it shows up with its exact version and launches correctly. From 7.7 onward Microsoft ships MSIX only, so this branch had to be covered.
- Polish: Expanding a file diff from the Changes panel into a center tab now uses the same background as the center area — previously there was a visible tint seam between the tab and the diff body.
- Polish: The folded "⋯ N unchanged lines" marker dropped its italic styling and switched to the app's unified UI font — the old mono-italic was fuzzy and hard to read for CJK and other locales; it's now crisp and clear.

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

- 修正:winget / Microsoft Store でインストールした PowerShell 7(MSIX パッケージ)が認識されるようになりました。設定 → ターミナル に PowerShell 7 の項目が以前は一切表示されませんでした——実体は WindowsApps 配下にロックされ、PATH には空の App Execution Alias しか置かれず、旧ロジックはそれを「未インストール」として除外していたためです。インストール場所を直接参照する方式に変更(VS Code の PowerShell 拡張と同様)し、インストールされていれば正確なバージョン付きで表示・起動できます。7.7 以降は MSIX のみになるため、このルートの対応は必須でした。
- 調整:変更記録でファイル比較を中央タブに展開した際、背景が中央エリアと完全に統一されました(以前はタブと比較内容の間に色の段差がありました)。
- 調整:折りたたまれた「⋯ N 行 変更なし」表示から斜体を外し、アプリ統一フォントに変更——以前の等幅斜体は CJK などでかすれて読みづらく、今は鮮明で読みやすくなりました。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

- 수정:winget / Microsoft Store 로 설치한 PowerShell 7(MSIX 패키지)이 이제 인식됩니다. 설정 → 터미널 에 PowerShell 7 항목이 아예 표시되지 않았습니다——실제 파일은 WindowsApps 에 잠겨 있고 PATH 에는 빈 껍데기 App Execution Alias 만 있어서, 이전 탐지 로직이 그것을 "설치 안 됨"으로 걸러냈기 때문입니다. 이제 설치 위치를 직접 참조하는 방식으로 바꾸어(VS Code PowerShell 확장과 동일), 설치되어 있으면 정확한 버전과 함께 표시되고 정상 실행됩니다. 7.7 부터는 MSIX 만 제공되므로 이 경로 대응은 필수였습니다.
- 개선:변경 기록에서 파일 비교를 중앙 탭으로 펼칠 때 배경이 중앙 영역과 완전히 통일되었습니다(이전에는 탭과 비교 내용 사이에 색 단차가 있었습니다).
- 개선:접힌 "⋯ N 행 변경 없음" 표시에서 이탤릭을 빼고 앱 통일 폰트로 변경——이전의 등폭 이탤릭은 CJK 등에서 번져서 읽기 어려웠고, 이제는 선명하게 읽힙니다.

</details>
