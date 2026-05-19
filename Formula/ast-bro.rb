class AstBro < Formula
  desc "Fast AST-based code-navigation toolkit: shape, surface, deps, search, pattern matching"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/2.1.0/ast-bro-macos-arm64.zip"
  sha256 "c0e3be2da5da9b40f6fc3f3568f6bbe5b7d73224085b7975a9d9fba3b0a2f598"
  license "MIT"

  def install
    bin.install "ast-bro"
    bin.install_symlink "ast-bro" => "ast-outline"
    bin.install_symlink "ast-bro" => "sb"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ast-bro --version")
  end
end
