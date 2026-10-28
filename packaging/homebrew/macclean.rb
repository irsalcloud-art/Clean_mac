# Homebrew formula for MacClean, meant for a `homebrew-macclean` tap.
# Installs the universal (arm64 + x86_64) release archive, pinned to its
# SHA-256 from the release's SHA256SUMS. See packaging/homebrew/README.md.
class Macclean < Formula
  desc "Storage analyzer, safe developer-cache cleaner and system monitor for macOS"
  homepage "https://github.com/irsalcloud-art/Clean_mac"
  url "https://github.com/irsalcloud-art/Clean_mac/releases/download/v0.1.4/macclean-0.1.4-universal-apple-darwin.tar.gz"
  sha256 "d3bd06fdad62782596ae94e4456eae8eddd15c1fac89e49804a774bd3ed97de8"
  license any_of: ["MIT", "Apache-2.0"]

  depends_on macos: :sequoia

  def install
    bin.install "macclean"
  end

  test do
    assert_equal "macclean #{version}", shell_output("#{bin}/macclean --version").strip
    # Read-only commands; nothing is deleted.
    assert_match "\"product_version\"", shell_output("#{bin}/macclean status --format json")
    system bin/"macclean", "clean", "--dry-run", "--category", "npm-cache"
  end
end
