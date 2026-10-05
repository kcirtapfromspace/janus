import Foundation
@testable import InterviewCoachKit
import XCTest

final class SetupTests: XCTestCase {
    /// The shape `ic setup status --json` prints for a fresh Mac (see setup.rs's tests).
    private let freshMac = """
    {"ready":false,"remaining":4,"model":"anthropic/claude-opus-5-5","checks":[
     {"id":"docker","status":"action","required":true,"title":"Install Docker Desktop","detail":"…",
      "action":{"label":"Get Docker Desktop","kind":"open_url","url":"https://www.docker.com/products/docker-desktop/"}},
     {"id":"proxy","status":"blocked","required":true,"title":"Set up the AI proxy","detail":"Needs Docker first.","action":null},
     {"id":"claude","status":"action","required":true,"title":"Sign in to Claude","detail":"…","action":{"label":"Sign in","kind":"sign_in"}},
     {"id":"models","status":"action","required":true,"title":"Download the speech models","detail":"About 1.7 GB",
      "action":{"label":"Download","kind":"run","step":"models"}},
     {"id":"typesafe_key","status":"optional","required":false,"title":"Add your TypeSafe (Jev) key (optional)","detail":"…",
      "action":{"label":"Add key","kind":"key","target":"typesafe"}}]}
    """

    func testDecodesEveryCheckAndAction() throws {
        let status = try ICClient.decode(SetupStatus.self, from: Data(freshMac.utf8))
        XCTAssertFalse(status.ready)
        XCTAssertEqual(status.remaining, 4)
        XCTAssertEqual(status.checks.map(\.status), [.action, .blocked, .action, .action, .optional])
        XCTAssertEqual(status.check("docker")?.action?.kind, .openURL(URL(string: "https://www.docker.com/products/docker-desktop/")!))
        XCTAssertNil(status.check("proxy")?.action)
        XCTAssertEqual(status.check("claude")?.action?.kind, .signIn)
        XCTAssertEqual(status.check("models")?.action?.kind, .run(step: "models"))
        XCTAssertEqual(status.check("typesafe_key")?.action?.kind, .key(target: "typesafe"))
    }

    /// A finished Mac: the rows still offer changes (switch account, restart the proxy).
    func testDecodesTheActionsOfFinishedRows() throws {
        let json = """
        {"ready":true,"remaining":0,"model":"anthropic/claude-opus-5-5","checks":[
         {"id":"proxy","status":"ok","required":true,"title":"AI proxy is running","detail":"…",
          "action":{"label":"Restart","kind":"run","step":"restart-proxy"}},
         {"id":"claude","status":"ok","required":true,"title":"Signed in to Claude as you@example.com (Your Org)","detail":"…",
          "action":{"label":"Switch Account…","kind":"switch_account"}}]}
        """
        let status = try ICClient.decode(SetupStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.check("proxy")?.action?.kind, .run(step: "restart-proxy"))
        XCTAssertEqual(status.check("claude")?.action?.kind, .switchAccount)
        XCTAssertEqual(status.check("claude")?.isDone, true)
    }

    func testDecodesChatGPTActionsAndAccountPickerWithoutCredentials() throws {
        let json = """
        {"ready":false,"remaining":1,"model":"openai/gpt-5.6","checks":[
          {"id":"openai","status":"action","required":true,"title":"Sign in with ChatGPT","detail":"…",
           "action":{"label":"Continue with ChatGPT","kind":"chat_gpt_sign_in","account":"oaiapp_one","new_account":false,"enable_plan":true}}],
         "openai":{"active":"oaiapp_one","email":"you@example.test","signed_in":true,"plan_enabled":false,"using_api_key":false,
           "accounts":[{"client_id":"oaiapp_one","label":"you@example.test · oaiapp_one"}]}}
        """
        let status = try ICClient.decode(SetupStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.check("openai")?.action?.kind, .chatGptSignIn(account: "oaiapp_one", newAccount: false, enablePlan: true))
        XCTAssertEqual(status.openai?.accounts.first?.clientId, "oaiapp_one")
        XCTAssertEqual(status.openai?.planEnabled, false)
        XCTAssertEqual(status.openai?.usingApiKey, false)
    }

    func testParsesEveryEvent() {
        XCTAssertEqual(SetupEvent.parse(#"{"event":"stage","message":"Downloading the speech model"}"#),
                       .stage("Downloading the speech model"))
        XCTAssertEqual(SetupEvent.parse(#"{"event":"progress","done":1048576,"total":1624555275}"#),
                       .progress(done: 1_048_576, total: 1_624_555_275))
        XCTAssertEqual(SetupEvent.parse(#"{"event":"open_url","url":"https://platform.claude.com/oauth/authorize?x=1"}"#),
                       .openURL(URL(string: "https://platform.claude.com/oauth/authorize?x=1")!))
        XCTAssertEqual(SetupEvent.parse(#"{"event":"need_code"}"#), .needCode)
        XCTAssertEqual(SetupEvent.parse(#"{"event":"done"}"#), .done)
        XCTAssertEqual(SetupEvent.parse(#"{"event":"error","message":"Docker didn't start"}"#), .error("Docker didn't start"))
        XCTAssertEqual(SetupEvent.parse(#"{"done":false,"event":"say","text":"What draws you to this role?"}"#),
                       .say("What draws you to this role?", done: false))
        XCTAssertEqual(SetupEvent.parse(#"{"done":true,"event":"say","text":"That's all my questions."}"#),
                       .say("That's all my questions.", done: true))
        XCTAssertNil(SetupEvent.parse(#"{"event":"say"}"#), "a turn with nothing to say")
        XCTAssertNil(SetupEvent.parse("Creating profile"), "anything that isn't an event is ignored")
    }

    func testLinesSplitAcrossReads() {
        var splitter = LineSplitter()
        XCTAssertEqual(splitter.feed(Data(#"{"event":"st"#.utf8)), [])
        XCTAssertEqual(splitter.feed(Data("age\"}\n{\"event\":\"done\"}\n{\"ev".utf8)), [#"{"event":"stage"}"#, #"{"event":"done"}"#])
        XCTAssertEqual(splitter.finish(), #"{"ev"#)
        XCTAssertNil(splitter.finish())
    }
}
