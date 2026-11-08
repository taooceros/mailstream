// mailstream-apple-tagger: the AI tagger's Apple Intelligence backend (`aiTagging.backend` =
// `apple`). The Rust engine (`Engine/src/omp.rs`) runs it like omp, once per batch:
//
//     mailstream-apple-tagger <instructions> <prompt>
//
// It asks Apple's on-device model (Foundation Models) for one verdict per conversation, by guided
// generation, and prints them as the JSON array omp answers with:
// [{"id":"1","category":"people","toRespond":true,"until":"2026-11-12","labels":["Work"]}, ...]
//
// Exit status: 0 with the array on stdout; otherwise one line for people on stderr and
// 3 when the model is unavailable (Apple Intelligence off, Mac not eligible, model not ready),
// 4 when the model declined the batch (its guardrails, a language it lacks; the engine splits
// the batch and retries), 1 for any other failure.
// scripts/apple-tagger.py builds it; the front ends bundle it beside their executable.
import Foundation
import FoundationModels

@Generable
enum Category: String {
    case people, notification, newsletter, invitation
}

@Generable
struct Verdict {
    @Guide(description: "The conversation's id from the input")
    var id: Int
    var category: Category
    // Asked before toRespond: naming the ask first keeps the small model from flagging mail that
    // only informs (measured on MS-140: 17/20 instead of 15/20 right).
    @Guide(description: "What the latest message asks the user personally to do (reply, pay, review, RSVP, check a sign-in), or \"nothing\" when it only informs: receipts, shipping, codes, statements, CI runs, promotions, FYI")
    var ask: String
    @Guide(description: "true only when ask is not nothing and the ask is still open today")
    var toRespond: Bool
    @Guide(description: "YYYY-MM-DD when toRespond is true, else null")
    var until: String?
    @Guide(description: "0-2 English Title Case topic words", .maximumCount(2))
    var labels: [String]
}

@Generable
struct Verdicts {
    @Guide(description: "One verdict per input conversation, in input order")
    var verdicts: [Verdict]
}

func fail(_ message: String, status: Int32) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(status)
}

let arguments = CommandLine.arguments
guard arguments.count == 3 else { fail("usage: mailstream-apple-tagger <instructions> <prompt>", status: 64) }

let model = SystemLanguageModel.default
switch model.availability {
case .available:
    break
case .unavailable(.appleIntelligenceNotEnabled):
    fail("Apple Intelligence is off; turn it on in System Settings > Apple Intelligence & Siri", status: 3)
case .unavailable(.deviceNotEligible):
    fail("this Mac cannot run Apple Intelligence; set aiTagging.backend to omp", status: 3)
case .unavailable(.modelNotReady):
    fail("Apple Intelligence's model is not ready yet (still downloading)", status: 3)
case .unavailable(let reason):
    fail("Apple Intelligence is unavailable (\(reason))", status: 3)
}

let session = LanguageModelSession(model: model, instructions: arguments[1])
do {
    let response = try await session.respond(
        to: arguments[2], generating: Verdicts.self, options: GenerationOptions(sampling: .greedy))
    let answer: [[String: Any]] = response.content.verdicts.map { verdict in
        ["id": String(verdict.id), "category": verdict.category.rawValue, "toRespond": verdict.toRespond,
         "until": verdict.until ?? NSNull(), "labels": verdict.labels]
    }
    FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: answer, options: [.sortedKeys]))
} catch let error as LanguageModelSession.GenerationError {
    switch error {
    case .guardrailViolation, .refusal, .unsupportedLanguageOrLocale:
        fail("Apple Intelligence declined to answer (\(error.localizedDescription))", status: 4)
    case .exceededContextWindowSize:
        fail("the batch is too long for Apple Intelligence; lower aiTagging.batchSize", status: 1)
    case .assetsUnavailable:
        fail("Apple Intelligence's model is not ready yet (still downloading)", status: 3)
    case .rateLimited, .concurrentRequests:
        fail("Apple Intelligence is busy; tagging retries later", status: 1)
    default:
        fail("Apple Intelligence failed: \(error.localizedDescription)", status: 1)
    }
} catch {
    fail("Apple Intelligence failed: \(error.localizedDescription)", status: 1)
}
