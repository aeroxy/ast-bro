class AstBro < Formula
  desc "Fast AST-based code navigation, search, rewrite, and log squeezing"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/4.5.0/ast-bro-macos-arm64.zip"
  sha256 "717f6b4ea885c343ace02240cefe43ff98021c1ec83d2ab5fbaf6f506236dd7d"
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
