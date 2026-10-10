class K9rs < Formula
  desc "Rust port of k9s, the Kubernetes terminal UI"
  homepage "https://github.com/natejsimonsen/k9rs"
  version "0.1.1"
  license "Apache-2.0"

  base = "https://github.com/natejsimonsen/k9rs/releases/download/v#{version}/k9rs-v#{version}"
  on_macos do
    on_arm do
      url "#{base}-aarch64-apple-darwin.tar.gz"
      sha256 "d294dd7612735655bf5c803c26195bbad98eb95e898c3ebeee90a34a3aeba57a"
    end
    on_intel do
      url "#{base}-x86_64-apple-darwin.tar.gz"
      sha256 "dcc03b938baf8bf35e868fdb5cb6ebc0b30b492bfb332cac2e489e3f17c92f99"
    end
  end
  on_linux do
    on_arm do
      url "#{base}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "389ae47665f8c1078e12cbd9fd1e46c83403114ac18b79923287e321a8362f84"
    end
    on_intel do
      url "#{base}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "3b1dc409fa3f48a2db04c15548d40cbe9224287d0f9111fe0033446955626bbb"
    end
  end

  def install
    bin.install "k9rs"
    generate_completions_from_executable(bin/"k9rs", "completion")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/k9rs version -s")
  end
end
