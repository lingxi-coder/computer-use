// Exercise the real active-context proof without a commit closure or input.
import Foundation
import ApplicationServices

@main
struct FixtureHitProbeMain {
    static func main() {
        guard CommandLine.arguments.count == 2,
              let data = try? Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])),
              data.count <= 8192,
              let request = try? JSONDecoder().decode(HitRequest.self, from: data),
              request.bundle_id == "com.lingxi.computer-user.mvp-fixture" else { exit(2) }
        let result = diagnosticHitTest(request, epoch: 7, requireActive: true, action: "ax_press")
        guard let output = try? JSONEncoder().encode(result) else { exit(2) }
        FileHandle.standardOutput.write(output)
        FileHandle.standardOutput.write(Data([10]))
    }
}
