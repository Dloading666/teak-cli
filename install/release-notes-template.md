<details open>
<summary><b>🇬🇧 English</b></summary>

- Coffee CLI now helps users whose machines were polluted by Orca by cleaning up the damaged config files. Orca's multi-agent design has been widely criticized — it silently writes status hooks into the config files of a dozen+ AI coding tools (Claude Code, Codex, Gemini, Kimi, Cursor, Grok…), which already ship complete status logic of their own. Once broken: hook errors on every session, Codex configs that fail to parse and refuse to start, and even config directories created for tools you never installed. Worse — uninstalling Orca does not remove the residue; it keeps breaking your tools, and non-technical users have almost no way to clean it up by hand. Now, launching Coffee CLI detects and removes this residue automatically: only Orca's entries are deleted, your own hooks stay untouched, and it skips entirely while Orca is running — no conflicts.

</details>

<details>
<summary><b>🇨🇳 简体中文</b></summary>

- Coffee CLI 现在会帮助被 Orca 工具污染的用户,处理被污染的配置文档。Orca 多 Agent 实现的模式饱受争议,存在大量"脏数据"——它会在十几个 AI 编程工具(Claude Code、Codex、Gemini、Kimi、Cursor、Grok…)的配置文件里自动写入状态钩子(hooks)。这些工具本来自带完整的状态逻辑,被 Orca 写坏后:每次会话报 hook 错误、Codex 配置直接无法解析启动失败、甚至给从没安装过的工具创建配置目录。更麻烦的是——卸载 Orca 后这些残留不会消失,会继续破坏你的工具,小白用户几乎不可能手工清理干净。现在启动 Coffee CLI 自动检测并清除这些残留:只删 Orca 写的东西,你自己的钩子配置原样保留;Orca 正在运行时自动跳过,绝不冲突。

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

- Coffee CLI 現在會幫助被 Orca 工具污染的用戶,處理被污染的設定檔。Orca 多 Agent 實作的方式備受爭議,存在大量「髒資料」——它會在十幾個 AI 程式設計工具(Claude Code、Codex、Gemini、Kimi、Cursor、Grok…)的設定檔中自動寫入狀態鉤子(hooks)。這些工具本來就內建完整狀態邏輯,被 Orca 寫壞後:每次對話回報 hook 錯誤、Codex 設定無法解析無法啟動、甚至替從沒安裝過的工具建立設定目錄。更麻煩的是——解除安裝 Orca 後這些殘留不會消失,會繼續破壞你的工具,新手使用者幾乎不可能手動清理乾淨。現在啟動 Coffee CLI 自動偵測並清除這些殘留:只刪 Orca 寫入的內容,你自己的鉤子設定原樣保留;Orca 正在執行時自動跳過,絕不衝突。

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

- Coffee CLI は Orca に汚染されたユーザーを支援し、破損した設定ファイルを整理します。Orca のマルチエージェント方式は物議を醸しており、大量の「汚染データ」を残します。十数種類の AI コーディングツール(Claude Code、Codex、Gemini、Kimi、Cursor、Grok など)の設定ファイルに状態フック(hooks)を自動で書き込みますが、これらのツールは本来、完全な状態ロジックを備えています。書き込みによって破損すると:毎セッションでフックエラーが発生し、Codex の設定が解析不能で起動できず、インストールしたことのないツール用の設定ディレクトリまで作られます。さらに厄介なのは、Orca をアンインストールしても残骸は消えず、ツールを壊し続け、一般ユーザーが手動で掃除するのはほぼ不可能なことです。Coffee CLI を起動すると、この残骸を自動検出して除去します。削除されるのは Orca が書き込んだものだけ。あなた自身のフック設定はそのまま残り、Orca 実行中は自動スキップされるため、競合しません。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

- Coffee CLI는 이제 Orca 도구에 오염된 사용자를 도와 손상된 설정 파일을 정리합니다. Orca의 다중 에이전트 구현 방식은 논란의 대상이며 다량의 '더러운 데이터'를 남깁니다. 열여러 개의 AI 코딩 도구(Claude Code, Codex, Gemini, Kimi, Cursor, Grok…) 설정 파일에 상태 훅(hooks)을 자동으로 주입합니다. 이 도구들은 원래 완전한 상태 로직을 갖추고 있는데, Orca가 손상시킨 뒤에는: 세션마다 훅 오류가 발생하고, Codex 설정이 파싱 불가로 시작되지 않으며, 설치한 적 없는 도구의 설정 디렉터리까지 생성됩니다. 더 골치 아픈 것은 Orca를 제거해도 잔여물이 사라지지 않고 계속 도구를 망가뜨린다는 점입니다. 일반 사용자가 수동으로 정리하기는 거의 불가능합니다. 이제 Coffee CLI를 시작하면 이런 잔여물을 자동으로 감지해 제거합니다. 삭제되는 것은 Orca가 작성한 것뿐이며, 사용자의 훅 설정은 그대로 보존됩니다. Orca가 실행 중이면 자동으로 건너뛰므로 충돌하지 않습니다.

</details>
