# Homebrew formula for claude-snatch
# To install: brew install claude-snatch/tap/claude-snatch
# Or manually: brew install --HEAD this-formula-path

class ClaudeSnatch < Formula
  desc "High-performance CLI/TUI tool for extracting Claude Code conversation logs"
  homepage "https://github.com/claude-snatch/claude-snatch"
  license "MIT"
  head "https://github.com/claude-snatch/claude-snatch.git", branch: "main"

  # For versioned releases, uncomment and update:
  # url "https://github.com/claude-snatch/claude-snatch/archive/refs/tags/v0.1.0.tar.gz"
  # sha256 "REPLACE_WITH_ACTUAL_SHA256"
  # version "0.1.0"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      Claude-snatch is installed!

      Usage:
        snatch list                    # List all projects and sessions
        snatch export SESSION_ID       # Export a session to markdown
        snatch stats --global          # View global usage statistics
        snatch tui                     # Launch interactive terminal UI

      The tool looks for Claude Code data in:
        macOS:   ~/.claude
        Linux:   ~/.claude
        Windows: %USERPROFILE%\\.claude

      For more information, see:
        snatch --help
        https://github.com/claude-snatch/claude-snatch
    EOS
  end

  test do
    assert_match "claude-snatch", shell_output("#{bin}/snatch --version")
    assert_match "Usage:", shell_output("#{bin}/snatch --help")
  end
end
