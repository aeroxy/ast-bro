class AstBro < Formula
  desc "Fast AST-based code navigation, search, rewrite, and log squeezing"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/4.0.0/ast-bro-macos-arm64.zip"
  sha256 "19e0e5066d8b972028b4ab88812df4ccdcb64e0fcd8ad18f81410fd4720e308a"
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
