class K9rs < Formula
  desc "Rust port of k9s, the Kubernetes terminal UI"
  homepage "https://github.com/natejsimonsen/k9rs"
  version "0.50.18"
  license "Apache-2.0"

  base = "https://github.com/natejsimonsen/k9rs/releases/download/v#{version}/k9rs-v#{version}"
  on_macos do
    on_arm do
      url "#{base}-aarch64-apple-darwin.tar.gz"
      sha256 "9e8afcade4a0b11a5eb3bd1b557e9d9859dad493bde90ccc011ec365a3fa2f95"
    end
    on_intel do
      url "#{base}-x86_64-apple-darwin.tar.gz"
      sha256 "d3345471d83a95c7d100c0b183ae6e3e3c6767403615910aee1fa9a133d6d671"
    end
  end
  on_linux do
    on_arm do
      url "#{base}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5788bea8b203934c8210750163b3333fdea8dba217ce49a7fbe969b18565e8d1"
    end
    on_intel do
      url "#{base}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "571ddf368dad251b41620db30bc03df1bec638d868f3be417666364cd6b76138"
    end
  end

  def install
    bin.install "k9rs"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/k9rs version -s")
  end
end
