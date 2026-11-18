#!/usr/bin/env python3
"""Fetch the open-source projects the VTComplete inline model is trained on.

Each project is a shallow clone of a public repository under a permissive
licence (MIT, Apache-2.0, BSD, ISC, zlib). `train` projects teach the model;
`holdout` projects are never trained on and measure it; `unseen` projects are
in languages the model is never shown at all. Nothing here ships with the
editor: only the trained weights do.

    python scripts/fetch_vtcomplete_inline_corpus.py DIRECTORY
"""
import argparse
import concurrent.futures
import pathlib
import subprocess
import sys

TRAIN = [
    # JavaScript and TypeScript
    "expressjs/express", "lodash/lodash", "prettier/prettier", "webpack/webpack",
    "nestjs/nest", "vuejs/core", "typeorm/typeorm", "excalidraw/excalidraw",
    "sveltejs/svelte", "facebook/react", "vitejs/vite", "denoland/std", "prisma/prisma",
    "element-plus/element-plus", "graphql/graphql-js",
    # Rust
    "tokio-rs/tokio", "BurntSushi/ripgrep", "serde-rs/serde", "clap-rs/clap",
    "rust-lang/cargo", "alacritty/alacritty", "sharkdp/bat", "bevyengine/bevy",
    "hyperium/hyper", "rust-lang/rust-analyzer",
    # Go
    "gin-gonic/gin", "gohugoio/hugo", "etcd-io/etcd", "caddyserver/caddy",
    "prometheus/prometheus", "junegunn/fzf", "docker/compose", "go-gitea/gitea", "golang/go",
    # Java and Kotlin
    "google/guava", "apache/commons-lang", "spring-projects/spring-boot",
    "Kotlin/kotlinx.coroutines", "ktorio/ktor", "square/retrofit", "netty/netty",
    "apache/kafka", "mockito/mockito", "square/okhttp", "JetBrains/Exposed",
    # C#
    "JamesNK/Newtonsoft.Json", "DapperLib/Dapper", "AutoMapper/AutoMapper",
    "dotnet/efcore", "NLog/NLog", "restsharp/RestSharp",
    # C, C++ and Objective-C
    "curl/curl", "libuv/libuv", "jqlang/jq", "nginx/nginx",
    "fmtlib/fmt", "nlohmann/json", "abseil/abseil-cpp", "catchorg/Catch2",
    "tmux/tmux", "lua/lua", "openssl/openssl", "raysan5/raylib",
    "google/googletest", "ocornut/imgui", "protocolbuffers/protobuf", "SFML/SFML",
    "AFNetworking/AFNetworking", "SDWebImage/SDWebImage",
    # Ruby, PHP, Swift, Elixir, Scala, Lua, shell, Dart
    "jekyll/jekyll", "rails/rails", "laravel/framework", "symfony/symfony",
    "apple/swift-nio", "vapor/vapor", "phoenixframework/phoenix", "elixir-lang/elixir",
    "playframework/playframework", "Kong/kong", "nvim-lua/plenary.nvim",
    "ohmyzsh/ohmyzsh", "dart-lang/http",
    "Homebrew/brew", "fastlane/fastlane", "rubocop/rubocop", "guzzle/guzzle", "slimphp/Slim",
    "filamentphp/filament", "sebastianbergmann/phpunit", "apple/swift-collections",
    "pointfreeco/swift-composable-architecture", "onevcat/Kingfisher", "flutter/packages",
    "rrousselGit/riverpod", "felangel/bloc", "scala/scala", "typelevel/cats",
    "elixir-ecto/ecto", "phoenixframework/phoenix_live_view", "LuaLS/lua-language-server",
    "neovim/neovim", "rbenv/rbenv", "dylanaraps/neofetch", "asdf-vm/asdf", "bash-it/bash-it",
    # Python
    "django/django", "pallets/flask", "pandas-dev/pandas", "scikit-learn/scikit-learn",
    "psf/black", "fastapi/fastapi", "numpy/numpy", "pytest-dev/pytest", "encode/httpx",
    "python/mypy", "pydantic/pydantic", "sqlalchemy/sqlalchemy",
    # Languages without a number of their own, markup, styles and configuration
    "haskell/cabal", "yesodweb/yesod", "PostgREST/postgrest", "ninenines/cowboy",
    "erlang/rebar3", "tidyverse/dplyr", "tidyverse/ggplot2", "fsprojects/FAKE",
    "giraffe-fsharp/Giraffe", "twbs/bootstrap", "tailwindlabs/tailwindcss",
    "h5bp/html5-boilerplate", "dahlbyk/posh-git", "PowerShell/PSReadLine",
    "terraform-aws-modules/terraform-aws-eks", "junegunn/vim-plug",
]

# A second, larger round of projects: the model stopped learning from the
# first list before it stopped having room to learn.
TRAIN += [
    # JavaScript and TypeScript
    "microsoft/TypeScript", "angular/angular", "vercel/next.js", "mui/material-ui",
    "storybookjs/storybook", "jestjs/jest", "babel/babel", "eslint/eslint", "microsoft/vscode",
    "socketio/socket.io", "reduxjs/redux", "TanStack/query", "remix-run/react-router",
    "mrdoob/three.js", "chartjs/Chart.js", "d3/d3", "fastify/fastify", "koajs/koa",
    "puppeteer/puppeteer", "microsoft/playwright", "yarnpkg/berry", "pnpm/pnpm", "nuxt/nuxt",
    "withastro/astro", "solidjs/solid", "preactjs/preact", "apollographql/apollo-client",
    "date-fns/date-fns", "immerjs/immer", "ant-design/ant-design",
    "typescript-eslint/typescript-eslint", "rollup/rollup", "mobxjs/mobx", "pmndrs/zustand",
    "colinhacks/zod", "supabase/supabase", "ionic-team/ionic-framework", "vuejs/vue",
    "vuetifyjs/vuetify", "cheeriojs/cheerio", "markedjs/marked", "moment/moment", "hapijs/hapi",
    "mochajs/mocha", "vitest-dev/vitest", "pixijs/pixijs", "electron/electron",
    "facebook/react-native", "mozilla/pdf.js",
    # Rust
    "rust-lang/rust", "denoland/deno", "astral-sh/ruff", "astral-sh/uv", "nushell/nushell",
    "starship/starship", "actix/actix-web", "tokio-rs/axum", "diesel-rs/diesel",
    "launchbadge/sqlx", "rayon-rs/rayon", "rust-lang/regex", "crossbeam-rs/crossbeam",
    "tauri-apps/tauri", "wasmerio/wasmer", "bytecodealliance/wasmtime", "seanmonstar/reqwest",
    "rust-lang/rustfmt", "rust-lang/rust-clippy", "sharkdp/fd", "swc-project/swc",
    "meilisearch/meilisearch", "tikv/tikv", "pola-rs/polars", "apache/arrow-rs",
    "apache/datafusion", "emilk/egui", "iced-rs/iced", "ratatui/ratatui", "hyperium/tonic",
    "rustls/rustls", "serde-rs/json", "dtolnay/syn", "tree-sitter/tree-sitter", "gfx-rs/wgpu",
    # Go
    "kubernetes/kubernetes", "moby/moby", "traefik/traefik", "golang/tools", "gorilla/mux",
    "labstack/echo", "gofiber/fiber", "spf13/viper", "go-gorm/gorm", "stretchr/testify",
    "uber-go/zap", "sirupsen/logrus", "grpc/grpc-go", "containerd/containerd", "helm/helm",
    "istio/istio", "pingcap/tidb", "influxdata/influxdb", "rclone/rclone", "cli/cli",
    "jesseduffield/lazygit", "charmbracelet/bubbletea", "go-chi/chi", "nats-io/nats-server",
    "kubernetes/client-go", "prometheus/alertmanager", "open-telemetry/opentelemetry-go",
    "gogs/gogs", "dgraph-io/badger", "etcd-io/bbolt", "redis/go-redis", "evanw/esbuild",
    "gruntwork-io/terragrunt", "JanDeDobbeleer/oh-my-posh",
    # Java, Kotlin and Groovy
    "spring-projects/spring-framework", "apache/commons-io", "apache/commons-collections",
    "apache/flink", "apache/spark", "apache/dubbo", "apache/maven", "apache/tomcat",
    "apache/lucene", "apache/cassandra", "google/guice", "google/dagger", "ReactiveX/RxJava",
    "FasterXML/jackson-databind", "FasterXML/jackson-core", "projectlombok/lombok",
    "mybatis/mybatis-3", "alibaba/fastjson2", "alibaba/druid", "apache/camel",
    "quarkusio/quarkus", "micronaut-projects/micronaut-core", "google/error-prone",
    "bazelbuild/bazel", "gradle/gradle", "square/moshi", "square/leakcanary",
    "apache/zookeeper", "apache/pulsar", "testcontainers/testcontainers-java",
    "assertj/assertj", "bumptech/glide", "greenrobot/EventBus", "PhilJay/MPAndroidChart",
    "jhy/jsoup", "libgdx/libgdx", "square/kotlinpoet", "InsertKoinIO/koin",
    "android/nowinandroid", "coil-kt/coil", "detekt/detekt", "arrow-kt/arrow",
    "JetBrains/compose-multiplatform", "kotest/kotest", "cashapp/sqldelight",
    "pinterest/ktlint", "spockframework/spock", "grails/grails-core", "apache/groovy",
    # C#, F# and PowerShell
    "dotnet/aspnetcore", "App-vNext/Polly", "jbogard/MediatR",
    "FluentValidation/FluentValidation", "xunit/xunit", "nunit/nunit", "moq/moq",
    "StackExchange/StackExchange.Redis", "dotnet/orleans", "MonoGame/MonoGame",
    "AvaloniaUI/Avalonia", "dotnet/msbuild", "PowerShell/PowerShell", "icsharpcode/ILSpy",
    "Humanizr/Humanizer", "npgsql/npgsql", "quartznet/quartznet",
    "ardalis/CleanArchitecture", "dotnet/BenchmarkDotNet", "reactiveui/ReactiveUI",
    "MassTransit/MassTransit", "unoplatform/uno", "mono/SkiaSharp", "dotnet/fsharp",
    "fsharp/FsAutoComplete", "fable-compiler/Fable", "fsprojects/Paket",
    "fsprojects/FSharp.Data", "SaturnFramework/Saturn", "fscheck/FsCheck", "pester/Pester",
    "dfinke/ImportExcel", "ScoopInstaller/Scoop", "dataplat/dbatools",
    "PowerShell/PSScriptAnalyzer",
    # C, C++ and Objective-C
    "sqlite/sqlite", "facebook/rocksdb", "facebook/folly", "google/flatbuffers", "grpc/grpc",
    "opencv/opencv", "bitcoin/bitcoin", "godotengine/godot", "microsoft/terminal",
    "madler/zlib", "libsdl-org/SDL", "glfw/glfw", "facebook/zstd", "google/re2",
    "google/benchmark", "gabime/spdlog", "CLIUtils/CLI11", "ninja-build/ninja",
    "envoyproxy/envoy", "apache/arrow", "duckdb/duckdb", "taskflow/taskflow", "skypjack/entt",
    "bulletphysics/bullet3", "assimp/assimp", "libevent/libevent", "nothings/stb",
    "DaveGamble/cJSON", "jemalloc/jemalloc", "microsoft/mimalloc", "h2o/h2o", "php/php-src",
    "python/cpython", "ruby/ruby", "lz4/lz4", "LuaJIT/LuaJIT", "micropython/micropython",
    "raspberrypi/pico-sdk", "love2d/love", "CocoaLumberjack/CocoaLumberjack",
    "SnapKit/Masonry", "ReactiveCocoa/ReactiveObjC", "ccgus/fmdb", "jdg/MBProgressHUD",
    "TextureGroup/Texture", "realm/realm-swift",
    # Python
    "pallets/click", "pallets/jinja", "scrapy/scrapy", "tornadoweb/tornado",
    "aio-libs/aiohttp", "celery/celery", "home-assistant/core", "matplotlib/matplotlib",
    "scipy/scipy", "sympy/sympy", "ipython/ipython", "pypa/pip", "python-poetry/poetry",
    "huggingface/transformers", "pytorch/pytorch", "keras-team/keras", "streamlit/streamlit",
    "fastapi/sqlmodel", "encode/django-rest-framework", "encode/starlette", "Textualize/rich",
    "Textualize/textual", "bokeh/bokeh", "plotly/plotly.py", "networkx/networkx", "boto/boto3",
    "PyCQA/flake8", "python-pillow/Pillow", "pallets/werkzeug", "dask/dask", "apache/airflow",
    "zulip/zulip", "mitmproxy/mitmproxy", "certbot/certbot", "locustio/locust",
    "pre-commit/pre-commit", "tqdm/tqdm", "marshmallow-code/marshmallow", "sphinx-doc/sphinx",
    "mkdocs/mkdocs", "httpie/cli", "langchain-ai/langchain", "openai/openai-python",
    "google/jax", "numba/numba", "pyca/cryptography",
    # Ruby and PHP
    "hanami/hanami", "heartcombo/devise", "rspec/rspec-core", "puma/puma", "Shopify/liquid",
    "rubygems/rubygems", "faker-ruby/faker", "rack/rack", "ruby-grape/grape", "spree/spree",
    "chef/chef", "capistrano/capistrano", "thoughtbot/factory_bot", "middleman/middleman",
    "solidusio/solidus", "laravel/laravel", "yiisoft/yii2", "cakephp/cakephp",
    "phpstan/phpstan-src", "doctrine/orm", "twigphp/Twig", "Seldaek/monolog",
    "briannesbitt/Carbon", "livewire/livewire", "nikic/PHP-Parser", "phalcon/cphalcon",
    "codeigniter4/CodeIgniter4", "thephpleague/flysystem", "vimeo/psalm",
    "PHP-CS-Fixer/PHP-CS-Fixer", "api-platform/core",
    # Swift and Dart
    "swiftlang/swift-package-manager", "apple/swift-algorithms", "apple/swift-argument-parser",
    "ReactiveX/RxSwift", "SnapKit/SnapKit", "realm/SwiftLint", "Moya/Moya",
    "SwiftyJSON/SwiftyJSON", "airbnb/lottie-ios", "danielgindi/Charts", "swiftlang/swift-syntax",
    "nicklockwood/SwiftFormat", "Quick/Quick", "swiftlang/swift-corelibs-foundation",
    "kean/Nuke", "groue/GRDB.swift", "hummingbird-project/hummingbird",
    "stephencelis/SQLite.swift", "flutter/flutter", "flutter/samples", "jonataslaw/getx",
    "cfug/dio", "flame-engine/flame", "fluttercommunity/plus_plugins", "rrousselGit/provider",
    # Scala, Elixir, Erlang, Haskell, Lua, R, shell, Vim script
    "apache/pekko", "scala/scala3", "zio/zio", "typelevel/cats-effect", "scalatest/scalatest",
    "sbt/sbt", "twitter/finagle", "slick/slick", "http4s/http4s", "circe/circe",
    "scalameta/metals", "elixir-plug/plug", "absinthe-graphql/absinthe", "oban-bg/oban",
    "elixir-nx/nx", "livebook-dev/livebook", "erlang/otp", "ninenines/ranch",
    "mochi/mochiweb", "michalmuskala/jason", "elixir-tesla/tesla", "elixir-lang/ex_doc",
    "haskell/haskell-language-server", "commercialhaskell/stack", "purescript/purescript",
    "haskell-servant/servant", "hasura/graphql-engine", "unisonweb/unison", "ndmitchell/hlint",
    "haskell/aeson", "snoyberg/conduit", "xmonad/xmonad", "nvim-telescope/telescope.nvim",
    "nvim-treesitter/nvim-treesitter", "neovim/nvim-lspconfig", "folke/lazy.nvim",
    "luarocks/luarocks", "leafo/lapis", "pkulchenko/ZeroBraneStudio", "apache/apisix",
    "hrsh7th/nvim-cmp", "mason-org/mason.nvim", "nvim-lualine/lualine.nvim", "tidyverse/tidyr",
    "tidyverse/purrr", "tidyverse/readr", "r-lib/testthat", "r-lib/devtools",
    "tidyverse/stringr", "r-lib/rlang", "tidymodels/parsnip", "r-lib/cli", "pyenv/pyenv",
    "romkatv/powerlevel10k", "zsh-users/zsh-autosuggestions",
    "zsh-users/zsh-syntax-highlighting", "bats-core/bats-core", "dokku/dokku", "tj/git-extras",
    "ko1nksm/shellspec", "vim-airline/vim-airline", "dense-analysis/ale", "neoclide/coc.nvim",
    "junegunn/fzf.vim",
    # Styles, interface definitions and configuration
    "necolas/normalize.css", "animate-css/animate.css", "jgthms/bulma",
    "foundation/foundation-sites", "primer/css", "saadeghi/daisyui", "picocss/pico",
    "googleapis/googleapis", "terraform-aws-modules/terraform-aws-vpc",
    "terraform-google-modules/terraform-google-kubernetes-engine",
]

HOLDOUT = [
    "axios/axios", "trpc/trpc", "spf13/cobra", "google/gson", "square/okio",
    "serilog/serilog", "google/leveldb", "sinatra/sinatra", "composer/composer",
    "Alamofire/Alamofire", "psf/requests", "nvm-sh/nvm",
]

# Languages that no training project is written in. The model has never seen
# them, so these measure how it copes with a language of the user's own.
UNSEEN = [
    "zigtools/zls", "tigerbeetle/tigerbeetle", "FluxML/Flux.jl", "JuliaLang/Pkg.jl",
    "nim-lang/nimble", "status-im/nim-chronos", "ocaml/dune", "kemalcr/kemal",
    "crystal-lang/shards", "gleam-lang/stdlib", "ring-clojure/ring",
]


def fetch(root, group, projects):
    def one(project):
        target = root / group / project.replace("/", "__")
        if target.exists():
            return None
        target.parent.mkdir(parents=True, exist_ok=True)
        # Neither the history nor large binary files are needed.
        command = [
            "git", "-c", "core.longpaths=true", "clone", "--quiet", "--depth", "1",
            "--filter=blob:limit=2m", f"https://github.com/{project}.git", str(target),
        ]
        if subprocess.run(command).returncode != 0:
            return project
        print(f"fetched {group}/{project}", flush=True)
        return None

    with concurrent.futures.ThreadPoolExecutor(6) as pool:
        return [project for project in pool.map(one, projects) if project]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("directory")
    arguments = parser.parse_args()
    root = pathlib.Path(arguments.directory)
    failed = fetch(root, "train", TRAIN) + fetch(root, "holdout", HOLDOUT) + fetch(root, "unseen", UNSEEN)
    if failed:
        print(f"could not fetch: {', '.join(failed)}")
    print("done")


if __name__ == "__main__":
    main()
