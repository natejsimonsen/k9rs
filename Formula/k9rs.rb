class K9rs < Formula
  desc "Rust port of k9s, the Kubernetes terminal UI"
  homepage "https://github.com/natejsimonsen/k9rs"
  version "0.1.0"
  license "Apache-2.0"

  base = "https://github.com/natejsimonsen/k9rs/releases/download/v#{version}/k9rs-v#{version}"
  on_macos do
    on_arm do
      url "#{base}-aarch64-apple-darwin.tar.gz"
      sha256 "e71c86a5e53ce46003f70a07620973a09f5310ed7fceb55fef5bfca8a4b4c315"
    end
    on_intel do
      url "#{base}-x86_64-apple-darwin.tar.gz"
      sha256 "60cc035ea1cf2d1dc1446318c71bb79c4fd6c1a0a9d13da722d6131063b57316"
    end
  end
  on_linux do
    on_arm do
      url "#{base}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e34f27cadd9cb1c37e9fdb57d724755a06762f4638028588848d9bffbc6d038b"
    end
    on_intel do
      url "#{base}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e1fe1f40377fbec16569ead3a842bf606b3beb8a7889ebe26d042e725e97225c"
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
