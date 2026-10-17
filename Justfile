tap := "../homebrew-spoon"

# Bump the version, test, then commit, tag and push; the tag starts .github/workflows/release.yml
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "{{version}}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3"; exit 1; }
    [[ -z "$(git status --porcelain)" ]] || { echo "commit or stash your changes first"; exit 1; }
    git rev-parse -q --verify "refs/tags/v{{version}}" >/dev/null && { echo "v{{version}} already exists"; exit 1; }
    perl -pi -e 's/^version = ".*"/version = "{{version}}"/' Cargo.toml
    cargo check -q # updates Cargo.lock
    cargo test -q
    git diff --quiet || git commit -am "Release v{{version}}"
    git tag "v{{version}}"
    git push origin HEAD "v{{version}}"

# Point the Homebrew formula at a pushed tag; review and open a PR in the tap yourself
tap version:
    #!/usr/bin/env bash
    set -euo pipefail
    url="https://github.com/DanWlker/spoon/archive/refs/tags/v{{version}}.tar.gz"
    sha=$(curl -fsSL "$url" | shasum -a 256 | cut -d ' ' -f 1)
    perl -pi -e "s{^  url \".*\"}{  url \"$url\"}; s{^  sha256 \".*\"}{  sha256 \"$sha\"}" "{{tap}}/Formula/spoon.rb"
    git -C "{{tap}}" diff

# Needs Hyprland (Lua config), kitty, wf-recorder, grim, ffmpeg and Maple Mono NF; runs on a temporary headless monitor
# Record img/demo.gif and the README screenshots, of a ratatui clone
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    img="{{justfile_directory()}}/img"
    bin="{{justfile_directory()}}/target/release/spoon"
    tmp=$(mktemp -d)
    repo=$tmp/ratatui
    sock=unix:$tmp/kitty.sock
    cargo build --release -q
    cleanup() {
        kitty @ --to $sock send-text q 2>/dev/null && sleep 1.5 || true
        hyprctl eval 'hl.window_rule({ name = "spoon-demo", enabled = false, match = { class = "spoondemo" } })' >/dev/null || true
        hyprctl output remove SPOONDEMO >/dev/null || true
        rm -rf "$tmp"
    }
    trap cleanup EXIT

    # a branch with a commit, a stash, and local changes; from a fixed commit, so the edits apply
    git clone -q https://github.com/ratatui/ratatui.git $repo
    cd $repo
    git config user.name Demo; git config user.email demo@example.com; git config commit.gpgsign false
    f=ratatui-widgets/src/scrollbar.rs
    git checkout -qb feat/scrollbar-demo d1ef782
    sed -i '0,/pub struct Scrollbar/s//\/\/\/ Scrollbars now remember their thumb position.\npub struct Scrollbar/' $f
    git commit -qam "feat(scrollbar): remember thumb position"
    echo "// wip" >> README.md && git stash -q -m "wip: README notes"
    sed -i 's/^rust-version = "1.88.0"/rust-version = "1.90.0"/' Cargo.toml
    sed -i '0,/track_symbol: Some(symbols.track),/s//track_symbol: Some(symbols.thumb),/' $f
    sed -i '0,/begin_symbol: Some(symbols.begin),/s//begin_symbol: None,/' $f
    [[ $(git diff --numstat | wc -l) == 2 ]] || { echo "the demo edits no longer apply"; exit 1; }
    printf 'Scrollbar demo notes\n- keep thumb position\n' > NOTES.md

    hyprctl output create headless SPOONDEMO >/dev/null; sleep 1
    hyprctl eval 'hl.monitor({ output = "SPOONDEMO", mode = "1600x1000@30", position = "20000x0", scale = 1 })' >/dev/null
    hyprctl eval 'hl.window_rule({ name = "spoon-demo", enabled = true, match = { class = "spoondemo" }, opacity = "1.0 override 1.0 override 1.0 override", no_blur = true, rounding = 0, border_size = 0, fullscreen = true, workspace = "11 silent" })' >/dev/null
    font="Maple Mono NF"
    fc-list -q "$font" || { echo "install $font first"; exit 1; }
    # HOME keeps your spoon and kitty config out; XDG_DATA_HOME keeps the fonts in it
    data=${XDG_DATA_HOME:-$HOME/.local/share}
    launch() {
        rm -f $tmp/kitty.sock*
        hyprctl eval "hl.exec_cmd(\"env HOME=$tmp XDG_DATA_HOME=$data kitty --class spoondemo --config NONE -o 'font_family=$font' -o allow_remote_control=yes --listen-on $sock -o background=#1e1e2e -o foreground=#cdd6f4 -o font_size=13 -o window_padding_width=6 -o confirm_os_window_close=0 -o enable_audio_bell=no --directory $repo $bin\")" >/dev/null
        sleep 4
    }
    # keys in <> are key names, others text; each followed by a pause of $D seconds
    k() {
        for a in "$@"; do
            case "$a" in
                \<*\>) kitty @ --to $sock send-key "${a:1:-1}" ;;
                *) kitty @ --to $sock send-text -- "$a" ;;
            esac
            sleep ${D:-0.6}
        done
    }
    shot() { sleep 1.2; grim -o SPOONDEMO "$img/$1.png"; }

    launch
    shot graph
    k '<tab>'; shot local-changes
    k P; shot popup
    k '<escape>' '<tab>' / '(#2818)' '<enter>' '<enter>' 3 j; shot details
    k '<escape>' '<escape>' 0 j l ']' l ']' l '[' '[' j j; shot refs
    # the blame and the timeline of a file, found in the files of the selected commit
    k '<escape>' 4 / 'ratatui-widgets/src/scrollbar.rs' '<enter>' B; sleep 1
    D=0.3 k j j j j j j j j; shot blame
    k '<escape>' T; sleep 1; k j j l; shot timeline
    k q; sleep 1.5

    # recorded losslessly: wf-recorder's default video marks limited-range colors as full range
    launch
    wf-recorder -o SPOONDEMO -r 30 -c ffv1 -x bgr0 -f $tmp/demo.mkv >/dev/null 2>&1 &
    rec=$!; sleep 3
    k '<tab>'; sleep 0.6
    D=0.5 k j j j; sleep 1.2; D=0.7 k '<enter>' '<enter>' j; k '<space>'; sleep 1.2
    k '<escape>' '<escape>' 3; sleep 0.4
    D=0.06 k f i x '(' s c r o l l b a r ')' ':' ' ' u s e ' ' t h u m b ' ' f o r ' ' t r a c k
    sleep 0.8; k '<escape>' c; sleep 1.8; k '<enter>'; sleep 1.8
    k '<tab>'; sleep 1.2; D=0.25 k j j j j j; sleep 0.5; k '<enter>'; sleep 1; k 3; sleep 1; k j; sleep 1.5
    k '<escape>' '<escape>'; k 0; sleep 0.5; D=0.8 k ']' ']'; sleep 0.8; k '<escape>'; sleep 0.5
    k 4; sleep 0.6; k / 'ratatui-widgets/src/scrollbar.rs'; sleep 0.6; k '<enter>' B; sleep 1.5
    D=0.3 k j j j j j j j j; sleep 0.6; k '['; sleep 1.5; k ']'; sleep 0.8; k '<escape>'
    k T; sleep 1.5; D=0.6 k j j; k l; sleep 1.5; D=0.4 k '<escape>' '<escape>' '<escape>' '<escape>'; sleep 0.8
    k P; sleep 2; k '<escape>'; sleep 1.5
    kill -INT $rec; wait $rec || true
    ffmpeg -v error -y -i $tmp/demo.mkv -vf "fps=12,scale=1200:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" "$img/demo.gif"
    ls -la "$img"/{demo.gif,graph.png,local-changes.png,popup.png,details.png,refs.png,blame.png,timeline.png}
