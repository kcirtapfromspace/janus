//! OpenAI via LiteLLM's OpenAI pass-through (`/openai_passthrough/v1/responses`), in OpenAI's
//! native Responses API: strict JSON-schema output and reasoning effort.

use std::io::BufRead;

use serde_json::{Value, json};

use super::{
    Llm, LlmError, StructuredRequest, error_from_response, for_each_event, http_client,
    with_retries,
};
use crate::proxy::LlmEndpoint;

const RESPONSES_PATH: &str = "/openai_passthrough/v1/responses";

pub fn request_body(req: &StructuredRequest) -> Value {
    json!({
        "model": req.model,
        "instructions": req.system,
        "input": req.user,
        "max_output_tokens": req.max_tokens,
        "stream": true,
        // Interview transcripts are personal: don't keep them on OpenAI's servers.
        "store": false,
        "reasoning": {"effort": req.effort.as_str()},
        "text": {"format": {"type": "json_schema", "name": req.schema_name, "strict": true, "schema": req.schema}},
    })
}

pub struct Client {
    http: reqwest::blocking::Client,
    endpoint: LlmEndpoint,
    token: Option<TokenSource>,
}

pub type TokenSource = Box<dyn Fn(bool) -> Result<String, LlmError>>;

impl Client {
    pub fn new(endpoint: LlmEndpoint) -> Self {
        Client {
            http: http_client(),
            endpoint,
            token: None,
        }
    }

    pub fn direct(token: TokenSource) -> Self {
        Client {
            http: http_client(),
            endpoint: LlmEndpoint {
                base_url: "https://api.openai.com".into(),
                api_key: String::new(),
            },
            token: Some(token),
        }
    }

    fn attempt(
        &self,
        body: &Value,
        on_progress: &mut dyn FnMut(usize),
    ) -> Result<String, LlmError> {
        let path = if self.token.is_some() {
            "/v1/responses"
        } else {
            RESPONSES_PATH
        };
        let token = match &self.token {
            Some(source) => source(false)?,
            None => self.endpoint.api_key.clone(),
        };
        let mut resp = self
            .http
            .post(format!(
                "{}{path}",
                self.endpoint.base_url.trim_end_matches('/')
            ))
            .header("content-type", "application/json")
            .bearer_auth(&token)
            .body(body.to_string())
            .send()
            .map_err(|e| LlmError::Network(e.to_string()))?;
        // One refresh on rejected OAuth access; never switch billing paths or loop sign-ins.
        if resp.status().as_u16() == 401
            && let Some(source) = &self.token
        {
            let fresh = source(true)?;
            resp = self
                .http
                .post(format!(
                    "{}{path}",
                    self.endpoint.base_url.trim_end_matches('/')
                ))
                .header("content-type", "application/json")
                .bearer_auth(fresh)
                .json(body)
                .send()
                .map_err(|e| LlmError::Network(e.to_string()))?;
        }
        if resp.status().as_u16() != 200 {
            return Err(error_from_response(resp));
        }
        parse_stream(std::io::BufReader::new(resp), on_progress)
    }
}

impl Llm for Client {
    fn structured(
        &self,
        req: &StructuredRequest,
        on_progress: &mut dyn FnMut(usize),
    ) -> Result<String, LlmError> {
        let body = request_body(req);
        with_retries(|| self.attempt(&body, on_progress))
    }
}

enum End {
    Completed,
    Incomplete(String),
}

/// Assemble the final text from a Responses API event stream.
pub fn parse_stream(
    reader: impl BufRead,
    on_progress: &mut dyn FnMut(usize),
) -> Result<String, LlmError> {
    let mut text = String::new();
    let mut refusal = String::new();
    let mut end: Option<End> = None;
    for_each_event(reader, |event| {
        match event["type"].as_str().unwrap_or_default() {
            // Reasoning items come before the message item; only output text is the answer.
            "response.output_text.delta" => {
                text.push_str(event["delta"].as_str().unwrap_or_default());
                on_progress(text.len());
            }
            "response.refusal.delta" => {
                refusal.push_str(event["delta"].as_str().unwrap_or_default())
            }
            "response.completed" => {
                end = Some(End::Completed);
                return Ok(false);
            }
            "response.incomplete" => {
                let reason = event["response"]["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or("unknown");
                end = Some(End::Incomplete(reason.to_string()));
                return Ok(false);
            }
            "response.failed" => {
                let error = &event["response"]["error"];
                let message = error["message"]
                    .as_str()
                    .or(error["code"].as_str())
                    .unwrap_or("response failed")
                    .to_string();
                return Err(super::provider_error(500, error["code"].as_str(), message));
            }
            "error" => {
                let code = event["code"].as_str().unwrap_or_default();
                let message = event["message"].as_str().unwrap_or(code).to_string();
                return Err(match code {
                    "rate_limit_exceeded" => LlmError::RateLimited,
                    "server_error" => LlmError::Api {
                        status: 500,
                        message,
                    },
                    _ => super::provider_error(400, Some(code), message),
                });
            }
            _ => {}
        }
        Ok(true)
    })?;
    match end {
        None => Err(LlmError::Network(
            "the stream ended before the model finished".into(),
        )),
        Some(End::Incomplete(reason)) if reason == "max_output_tokens" => Err(LlmError::MaxTokens),
        Some(End::Incomplete(reason)) if reason == "content_filter" => {
            Err(LlmError::Refusal("content filter".into()))
        }
        Some(End::Incomplete(reason)) => {
            Err(LlmError::Protocol(format!("response incomplete: {reason}")))
        }
        Some(End::Completed) if !refusal.is_empty() => Err(LlmError::Refusal(refusal)),
        Some(End::Completed) if text.trim().is_empty() => {
            Err(LlmError::Protocol("no text in the response".into()))
        }
        Some(End::Completed) => Ok(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Effort, sse};

    fn delta(text: &str, output_index: u32) -> Value {
        json!({"type": "response.output_text.delta", "output_index": output_index, "content_index": 0, "delta": text})
    }

    #[test]
    fn native_requests_use_public_responses_and_refresh_once_on_401() {
        use std::cell::RefCell;
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::rc::Rc;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let mut requests = vec![];
            for status in [401, 200] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    headers.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                requests.push((
                    headers.to_lowercase(),
                    serde_json::from_slice::<Value>(&body).unwrap(),
                ));
                let body = if status == 401 {
                    "{}".into()
                } else {
                    sse(&[delta("{}", 0), json!({"type":"response.completed"})])
                };
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        let seen = Rc::new(RefCell::new(vec![]));
        let observed = seen.clone();
        let client = Client {
            http: http_client(),
            endpoint: LlmEndpoint {
                base_url,
                api_key: "never-use-this-proxy-key".into(),
            },
            token: Some(Box::new(move |force| {
                observed.borrow_mut().push(force);
                Ok(if force {
                    "refreshed-token"
                } else {
                    "initial-token"
                }
                .into())
            })),
        };
        let schema =
            json!({"type":"object", "properties":{}, "required":[], "additionalProperties":false});
        let request = StructuredRequest {
            model: "gpt-5.6",
            system: "sys",
            user: "example transcript",
            schema: &schema,
            schema_name: "test",
            effort: Effort::Low,
            max_tokens: 16,
        };
        assert_eq!(client.structured(&request, &mut |_| {}).unwrap(), "{}");
        assert_eq!(*seen.borrow(), [false, true]);
        let requests = task.join().unwrap();
        for (headers, body) in &requests {
            assert!(headers.starts_with("post /v1/responses "));
            assert!(!headers.contains("litellm") && !headers.contains("never-use-this-proxy-key"));
            assert_eq!(body["store"], false);
            assert_eq!(body["stream"], true);
        }
        assert!(
            requests[0]
                .0
                .contains("authorization: bearer initial-token")
        );
        assert!(
            requests[1]
                .0
                .contains("authorization: bearer refreshed-token")
        );
    }

    #[test]
    fn chatgpt_plan_limits_in_a_started_stream_stop_without_retrying_or_switching_billing() {
        let data = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}}\n\n";
        let err = parse_stream(std::io::Cursor::new(data), &mut |_| {}).unwrap_err();
        assert!(matches!(err, LlmError::PlanLimit));
        assert!(!err.retryable());
        assert!(matches!(
            super::super::provider_error(
                403,
                Some("subscription_sharing_user_not_eligible"),
                String::new()
            ),
            LlmError::PlanUnavailable
        ));
    }

    #[test]
    fn parses_text_deltas_after_reasoning_items() {
        let stream = sse(&[
            json!({"type": "response.created", "response": {"status": "in_progress"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning"}}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message"}}),
            delta("{\"a\":", 1),
            delta("1}", 1),
            json!({"type": "response.completed", "response": {"status": "completed"}}),
        ]);
        let mut seen = vec![];
        assert_eq!(
            parse_stream(stream.as_bytes(), &mut |n| seen.push(n)).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(seen.last(), Some(&7));
    }

    #[test]
    fn a_stream_cut_off_mid_answer_is_a_retryable_error() {
        let cut = sse(&[delta("{\"summ", 0)]);
        let err = parse_stream(cut.as_bytes(), &mut |_| {}).unwrap_err();
        assert!(
            matches!(err, LlmError::Network(_)) && err.retryable(),
            "{err}"
        );
    }

    #[test]
    fn incomplete_refused_and_failed_responses_surface() {
        let incomplete = |reason: &str| {
            sse(&[
                delta("{", 0),
                json!({"type": "response.incomplete",
                                         "response": {"incomplete_details": {"reason": reason}}}),
            ])
        };
        assert!(matches!(
            parse_stream(incomplete("max_output_tokens").as_bytes(), &mut |_| {}),
            Err(LlmError::MaxTokens)
        ));
        assert!(matches!(
            parse_stream(incomplete("content_filter").as_bytes(), &mut |_| {}),
            Err(LlmError::Refusal(_))
        ));

        let refused = sse(&[
            json!({"type": "response.refusal.delta", "delta": "I can't help with that."}),
            json!({"type": "response.completed", "response": {"status": "completed"}}),
        ]);
        let err = parse_stream(refused.as_bytes(), &mut |_| {}).unwrap_err();
        assert!(
            matches!(err, LlmError::Refusal(ref r) if r.contains("can't help")),
            "{err}"
        );

        let failed = sse(&[
            json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}}),
        ]);
        assert!(
            parse_stream(failed.as_bytes(), &mut |_| {})
                .unwrap_err()
                .retryable()
        );

        let limited =
            sse(&[json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"})]);
        assert!(matches!(
            parse_stream(limited.as_bytes(), &mut |_| {}),
            Err(LlmError::RateLimited)
        ));
    }

    #[test]
    fn request_body_uses_native_responses_features() {
        let schema = json!({"type": "object"});
        let body = request_body(&StructuredRequest {
            model: "gpt-5.6",
            system: "sys",
            user: "hi",
            schema: &schema,
            schema_name: "session_analysis",
            effort: Effort::High,
            max_tokens: 64000,
        });
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["input"], "hi");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        let format = &body["text"]["format"];
        assert_eq!(
            (
                format["type"].as_str(),
                format["name"].as_str(),
                format["strict"].as_bool()
            ),
            (Some("json_schema"), Some("session_analysis"), Some(true))
        );
    }
}
