<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-dark.svg">
    <img alt="Elisa" src="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-light.svg" width="360">
  </picture>
</p>

<p align="center"><b>あなたの Mac に住み、あなたのモデルで動くパーソナルアシスタント。</b></p>

<p align="center"><a href="README.md">English</a> · <a href="README.vi.md">Tiếng Việt</a> · <b>日本語</b></p>

<p align="center">
  <a href="https://github.com/transybao1393/Elisa/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/transybao1393/Elisa?style=flat&logo=github"></a>
  <a href="https://github.com/transybao1393/Elisa/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/transybao1393/Elisa?include_prereleases&sort=semver"></a>
  <a href="https://github.com/transybao1393/Elisa/commits/main"><img alt="Last commit" src="https://img.shields.io/github/last-commit/transybao1393/Elisa"></a>
  <a href="https://github.com/transybao1393/Elisa/issues"><img alt="Open issues" src="https://img.shields.io/github/issues/transybao1393/Elisa"></a>
  <img alt="Platform: macOS 14+" src="https://img.shields.io/badge/platform-macOS%2014%2B-black?logo=apple">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-core-orange?logo=rust">
  <img alt="SwiftUI" src="https://img.shields.io/badge/SwiftUI-app-F05138?logo=swift&logoColor=white">
  <img alt="MCP" src="https://img.shields.io/badge/MCP-plugins-6E56CF">
  <img alt="Local first" src="https://img.shields.io/badge/local--first-private-2EA44F">
  <a href="LICENSE"><img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache%202.0-blue"></a>
</p>


Elisa は Mac のための小さなチャットアシスタントです。あなたが選んだモデルで動き、macOS アプリでもターミナルでも使えます。
Elisa が今何をしているかを見せてくれる小さな相棒、Shade も一緒です。ローカルモデルなら、何も Mac の外に出ません。

## 機能

- **チャット**：どの言語でも。質問、文章作成、計画、ジョーク。簡単な質問には、ツールを使わずに簡単に答えます。
- **選んだモデルに合わせる。** 起動時にモデルができること（画像認識、ツール呼び出し、思考、コンテキスト長）を検出し、
  それに合わせて動きます。ネイティブのツール呼び出しがないモデルでも、プロンプト経由でツールを使えます。
  小さなコンテキストも自動で扱います。
- **会話の途中でモデルを切り替え。** 会話はそのまま引き継がれ、新しいモデルは自分自身として答えます。
- **Web。** 新しい情報が必要な質問では検索し、ページを読みます。結果は 1 時間キャッシュされます。
- **ファイルと文書。** プロジェクトフォルダ内のファイルを読み、保存します。添付された PDF、スプレッドシート、
  Word ファイルを理解します。
- **画像**：画像認識モデルで、続けての質問にも対応。画像を見られないモデルは、推測せずにはっきりそう伝えます。
- **記憶。** 伝えたことを覚えます。`/pin` で事実を保持、`/notes` で保持内容を表示、`/forget` で消去。
  長い会話は自動で要約されます（`/compact` でも可）。前回の会話は、望むときに続けられます。
- **時刻と日付** は Mac の時計から答えます。
- **段階的な推論** ツール（think、steps、calc）。必要な質問のときだけ使います。
- **コーディング支援**：数ある機能のひとつとして。承認のもとでプロジェクトを読んで編集し、コマンドを実行し、
  オフラインのサンドボックス（Python、JavaScript、Rust、Go、Bash）でコード片を試せます。
- **インテントルーター（オプション）。** 各メッセージに必要なツールを判断する、サーバー上のモデル（有料）。
  なくても Elisa は自分でルーティングします。

## プライベートで、あなたの管理下に

- **モデルはあなたが選ぶ。** Ollama、LM Studio、llama.cpp、MLX、または OpenAI / Anthropic 互換の任意の API。
  Elisa がモデルを勝手に選ぶことはありません。Model Scorecard が実測結果を示し、選ぶ手助けをします。
- **ローカルファースト。** ローカルモデルなら、会話は Mac の中に留まります。
- **知らないところで何も起きない。** 読み取りは自由。ファイルの書き込み、コマンドの実行、現在のフォルダ外への操作は
  事前に確認します。許可 / 確認 / 拒否のルールで一度決めれば済みます。
- **最初から安全。** 貼り付けた秘密情報はオンラインモデルとディスクへの保存から隠され、Web と文書の内容は
  信頼できないものとして扱われ、すべての操作は改ざん検知付きの監査ログに記録されます。
- **会話は毎回まっさらから。** 新しいチャットは、そこで尋ねたことに従います。以前の会話は、選んだときだけ続けます。

## 拡張する

Elisa が使えるものはすべて MCP でつながります。組み込みサーバーは Web 検索、文書、推論、コードサンドボックスを
カバーします。プラグインはローカルフォルダから MCP サーバー、スラッシュコマンド、権限ルールをまとめて追加し、
Mac が必要な場合を除き、オフラインのコンテナで動きます。

## クイックスタート

必要なもの：macOS 14 以降、Rust と Swift のツールチェーン。Colima はオプションで、コードサンドボックスと
コンテナプラグインに使います。

```sh
# macOS アプリ（必要なものはすべて自動でビルド）
app/ElisaApp/run.sh                      # モデルを選んで、チャットを開始

# ターミナルアプリ
cd elisa && cargo build --release        # バイナリ：elisa/target/release/elisa
elisa/target/release/elisa               # ターミナルでチャット
```

## リポジトリ

| パス | 内容 |
|---|---|
| [`elisa/`](elisa/) | Elisa 本体（Rust）：チャット、ツール、MCP ホストと組み込みサーバー、プラグイン、評価。詳しいドキュメントは [`elisa/README.md`](elisa/README.md)。 |
| [`app/ElisaApp/`](app/ElisaApp/) | macOS ネイティブアプリ（SwiftUI）。 |
| [`app/ElisaCompanion/`](app/ElisaCompanion/) | Shade の小さなフローティングウィンドウ。 |
| [`designs/`](designs/) | アプリアイコンとロゴ。 |

## ライセンス

Apache License 2.0。Copyright 2026 TRAN SY BAO. [LICENSE](LICENSE)、[NOTICE](NOTICE)、
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。このライセンスは Elisa の名称、ロゴ、Shade の使用を許可しません。
モデルの重み（サーバー上のインテントルーターを含む）はこのリポジトリに含まれず、それぞれのライセンスに従います。
