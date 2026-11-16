class AstBro < Formula
  desc "Fast AST-based code navigation, search, rewrite, and log squeezing"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/4.4.0/ast-bro-macos-arm64.zip"
  sha256 "a8fda2e54418ee9183a6e3210145523fe16a1adfb45d4753ce5d00fda1670431"
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
