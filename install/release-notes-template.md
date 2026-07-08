<details open>
<summary><b>🇨🇳 简体中文</b></summary>

- 新增:Gambit 输入框支持 ↑/↓ 翻阅历史提示词。↑ 回填上一条,↓ 往新走,越过最新一条会恢复你按↑前正在敲的内容(不会丢失半截输入)。多行草稿只在首行/末行触发,中间行保留原生光标移动。历史全局共享、本地持久化、连续重复去重。
- 新增:设置 → 终端 里可选默认 Shell。Windows 支持 Auto / pwsh / PowerShell / Git Bash / cmd(标"不推荐");macOS/Linux 支持 Auto / zsh / bash / fish / sh。只显示已安装的 shell,没装的不显示(避免选了死终端)。
- 修复:从 Microsoft Store 装的 pwsh 之前会启动失败(0 字节 App Execution Alias,探测能过但 spawn 报 ERROR_ACCESS_DEAD)。现在解析到真实绝对路径再启动,该 bug 不再出现。

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

- 新增:Gambit 輸入框支援 ↑/↓ 翻閱歷史提示詞。↑ 回填上一條,↓ 往新走,越過最新一條會恢復你按↑前正在敲的內容(不會丟失半截輸入)。多行草稿只在首行/末行觸發,中間行保留原生游標移動。歷史全域共享、本地持久化、連續重複去重。
- 新增:設定 → 終端 裡可選預設 Shell。Windows 支援 Auto / pwsh / PowerShell / Git Bash / cmd(標「不推薦」);macOS/Linux 支援 Auto / zsh / bash / fish / sh。只顯示已安裝的 shell,沒裝的不顯示(避免選了死終端)。
- 修復:從 Microsoft Store 裝的 pwsh 之前會啟動失敗(0 位元組 App Execution Alias,探測能過但 spawn 報 ERROR_ACCESS_DENIED)。現在解析到真實絕對路徑再啟動,該 bug 不再出現。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

- New: Gambit compose box now recalls previous prompts with ↑/↓. ↑ fills the last sent prompt, ↓ walks back toward the present; scrolling ↓ past the newest restores the draft you were typing before pressing ↑ (never lose a half-typed prompt). Multi-line drafts only trigger recall on the first/last line; mid-text ↑/↓ stays native caret movement. History is global, localStorage-persisted, and dedupes consecutive repeats.
- New: Choose your default shell in Settings → Terminal. Windows: Auto / pwsh / PowerShell / Git Bash / cmd (marked "not recommended"); macOS/Linux: Auto / zsh / bash / fish / sh. Only installed shells are shown — a missing one never appears (no dead-terminal picks).
- Fixed: pwsh installed from the Microsoft Store previously failed to launch (0-byte App Execution Alias — detection passed but spawn returned ERROR_ACCESS_DENIED). Now resolved to the real absolute path before spawn; the bug no longer occurs.

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

- 新機能:Gambit 入力ボックスで ↑/↓ による過去のプロンプト呼び出しに対応。↑ で直前のプロンプトを再入力、↓ で新しい方へ戻り、最新を越えると↑を押す前に打っていた下書きが復元されます(入力途中の内容は失われません)。複数行の下書きは先頭/末尾行でのみ発動し、中間行ではネイティブのキャレット移動を維持。履歴は全タブ共有・ローカル永続化・連続重複は除去されます。
- 新機能:設定 → ターミナル でデフォルトシェルを選択可能。Windows: Auto / pwsh / PowerShell / Git Bash / cmd(「非推奨」表示);macOS/Linux: Auto / zsh / bash / fish / sh。インストール済みのシェルのみ表示 — 未インストールのものは出現せず(死んだターミナルを選ぶ心配なし)。
- 修正:Microsoft Store 版 pwsh が起動失敗していた問題を修正(0 バイトの App Execution Alias — 検出は通るが spawn が ERROR_ACCESS_DENIED)。起動前に実際の絶対パスへ解決するようにし、本バグは再発しません。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

- 신규:Gambit 입력란에서 ↑/↓ 로 과거 프롬프트 불러오기 지원. ↑ 는 직전 프롬프트를 채워 넣고, ↓ 는 최신 쪽으로 이동하며, 최신을 넘기면 ↑ 를 누르기 전에 입력하던 내용이 복원됩니다(입력 중이던 내용 유실 없음). 여러 줄 초안은 첫 줄/마지막 줄에서만 동작하고 중간 줄은 네이티브 캐럿 이동을 유지. 기록은 전역 공유·로컬 영구화·연속 중복 제거됩니다.
- 신규:설정 → 터미널 에서 기본 셸 선택 가능. Windows: Auto / pwsh / PowerShell / Git Bash / cmd(「권장하지 않음」 표시);macOS/Linux: Auto / zsh / bash / fish / sh. 설치된 셸만 표시 — 설치되지 않은 것은 나타나지 않아(죽은 터미널 선택 걱정 없음).
- 수정:Microsoft Store 설치 pwsh 가 실행에 실패하던 문제 수정(0바이트 App Execution Alias — 탐지는 통과하지만 spawn 이 ERROR_ACCESS_DENIED). 실행 전 실제 절대 경로로 해석하도록 수정, 본 버그는 재발하지 않습니다.

</details>
