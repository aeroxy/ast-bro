class AstBro < Formula
  desc "Fast AST-based code navigation, search, rewrite, and log squeezing"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/4.1.0/ast-bro-macos-arm64.zip"
  sha256 "38a1047b396212e5c933447c9d95898b21a0629dbc1e6232bc6fde8f106463f6"
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
