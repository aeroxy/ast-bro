# Pinned to the last pre-rename release. New versions ship as ast-bro.rb.
# Kept for backward compatibility — existing `brew install ast-outline` users
# will continue to get this formula.
class AstOutline < Formula
  desc "Fast AST-based code-navigation toolkit: shape, surface, deps, search"
  homepage "https://github.com/aeroxy/ast-outline"
  url "https://github.com/aeroxy/ast-outline/releases/download/2.1.0/ast-outline-macos-arm64.zip"
  sha256 "c0e3be2da5da9b40f6fc3f3568f6bbe5b7d73224085b7975a9d9fba3b0a2f598"
  license "MIT"

  def install
    bin.install "ast-outline"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ast-outline --version")
  end
end
