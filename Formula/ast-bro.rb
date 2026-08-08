class AstBro < Formula
  desc "Fast AST-based code navigation, search, rewrite, and log squeezing"
  homepage "https://github.com/aeroxy/ast-bro"
  url "https://github.com/aeroxy/ast-bro/releases/download/4.2.0/ast-bro-macos-arm64.zip"
  sha256 "20a8d52b7db514250ab4164850ecae49a0f48ef8a0be6d1c1f1e9c32377f98ea"
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
