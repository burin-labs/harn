#!/usr/bin/env bash
# Install one daily host job for the current target-cache collector.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
installed="${HARN_TARGET_GC_INSTALL_DIR:-$HOME/.local/bin}/harn-target-gc-maintenance"
log_dir="${HARN_TARGET_GC_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/harn}"
log_path="$log_dir/target-gc-maintenance.log"
marker="# harn-target-gc-maintenance"

mkdir -p "$(dirname "$installed")" "$log_dir"
install -m 0755 "$script_dir/target_gc_maintenance.sh" "$installed"

read_crontab() {
  local existing
  if existing="$(crontab -l 2>&1)"; then
    printf '%s' "$existing"
    return 0
  fi
  case "$existing" in
    *"no crontab for"*) return 0 ;;
    *) echo "harn-target maintenance: cannot read existing crontab: $existing" >&2; return 1 ;;
  esac
}

install_cron() {
  local existing without_old installed_shell log_shell entry installed_tab
  existing="$(read_crontab)"
  without_old="$(printf '%s\n' "$existing" | awk -v marker="$marker" 'index($0, marker) == 0')"
  printf -v installed_shell '%q' "$installed"
  printf -v log_shell '%q' "$log_path"
  entry="17 3 * * * $installed_shell >> $log_shell 2>&1 $marker"
  if ! printf '%s\n%s\n' "$without_old" "$entry" | crontab -; then
    echo "harn-target maintenance: daily cron write failed" >&2
    return 1
  fi
  installed_tab="$(read_crontab)"
  if [ "$(printf '%s\n' "$installed_tab" | grep -Fc "$marker")" -ne 1 ] \
    || ! grep -Fq "$entry" <<< "$installed_tab"; then
    echo "harn-target maintenance: daily cron job did not read back" >&2
    return 1
  fi
  echo "harn-target maintenance: installed daily cron job and current-policy wrapper"
}

xml_escape() {
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"
}

remove_legacy_cron() {
  local existing without_old installed_tab
  existing="$(read_crontab)"
  if ! grep -Fq "$marker" <<< "$existing"; then
    return 0
  fi
  without_old="$(printf '%s\n' "$existing" | awk -v marker="$marker" 'index($0, marker) == 0')"
  if ! printf '%s\n' "$without_old" | crontab -; then
    echo "harn-target maintenance: legacy cron job could not be removed" >&2
    return 1
  fi
  installed_tab="$(read_crontab)"
  if grep -Fq "$marker" <<< "$installed_tab"; then
    echo "harn-target maintenance: legacy cron job remains after removal" >&2
    return 1
  fi
}

install_launchd() {
  local label=com.harn.target-gc-maintenance
  local domain="gui/$(id -u)"
  local launchagents="$HOME/Library/LaunchAgents"
  local plist="$launchagents/$label.plist"
  local staged readback
  mkdir -p "$launchagents"
  staged="$(mktemp "$launchagents/.harn-target-gc.XXXXXX")"
  cat > "$staged" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array><string>$(xml_escape "$installed")</string></array>
<key>StartCalendarInterval</key><dict><key>Hour</key><integer>3</integer><key>Minute</key><integer>17</integer></dict>
<key>StandardOutPath</key><string>$(xml_escape "$log_path")</string>
<key>StandardErrorPath</key><string>$(xml_escape "$log_path")</string>
</dict></plist>
EOF
  if ! plutil -lint "$staged" >/dev/null; then
    rm -f -- "$staged"
    echo "harn-target maintenance: LaunchAgent plist validation failed" >&2
    return 1
  fi

  if launchctl print "$domain/$label" >/dev/null 2>&1; then
    if cmp -s "$staged" "$plist"; then
      rm -f -- "$staged"
    else
      if ! launchctl bootout "$domain/$label"; then
        rm -f -- "$staged"
        echo "harn-target maintenance: old LaunchAgent could not be unloaded" >&2
        return 1
      fi
      mv -f -- "$staged" "$plist"
      launchctl bootstrap "$domain" "$plist"
    fi
  else
    mv -f -- "$staged" "$plist"
    launchctl bootstrap "$domain" "$plist"
  fi

  if ! readback="$(launchctl print "$domain/$label")" \
    || ! grep -Fq "path = $plist" <<< "$readback"; then
    echo "harn-target maintenance: daily LaunchAgent did not read back" >&2
    return 1
  fi
  if ! remove_legacy_cron; then
    # Keep the legacy schedule as the fallback if its marker cannot be
    # removed. A loaded LaunchAgent beside it would run the same sweep twice.
    if ! launchctl bootout "$domain/$label"; then
      echo "harn-target maintenance: duplicate schedule could not be unloaded" >&2
      return 1
    fi
    rm -f -- "$plist"
    return 1
  fi
  echo "harn-target maintenance: installed daily LaunchAgent and current-policy wrapper"
}

case "$(uname -s)" in
  Darwin) install_launchd ;;
  Linux) install_cron ;;
  *) echo "harn-target maintenance: unsupported scheduler platform" >&2; exit 1 ;;
esac
