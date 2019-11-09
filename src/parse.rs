use imap_proto::{self, MailboxDatum, RequestId, Response};
use std::collections::HashSet;

use async_std::prelude::*;
use async_std::stream::Stream;
use async_std::sync;

use super::error::Result;
use super::types::*;
use crate::codec::ResponseData;

pub(crate) fn parse_names<'a, T: Stream<Item = ResponseData> + Unpin>(
    stream: &'a mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> impl Stream<Item = Result<Name<'a>>> + 'a {
    use futures::StreamExt;

    StreamExt::filter_map(
        StreamExt::take_while(stream, move |res| match res.parsed() {
            Response::Done { tag, .. } => futures::future::ready(&command_tag != tag),
            _ => futures::future::ready(true),
        }),
        move |resp| {
            let unsolicited = unsolicited.clone();

            async move {
                match resp.parsed() {
                    Response::MailboxData(MailboxDatum::List {
                        flags,
                        delimiter,
                        name,
                    }) => Some(Ok(Name {
                        attributes: flags
                            .into_iter()
                            .map(|s| NameAttribute::from((*s).to_string()))
                            .collect(),
                        delimiter: (*delimiter).map(Into::into),
                        name: (*name).into(),
                    })),
                    _resp => match handle_unilateral(&resp, unsolicited).await {
                        Some(resp) => match resp.parsed() {
                            Response::Fetch(..) => None,
                            resp => Some(Err(resp.into())),
                        },
                        None => None,
                    },
                }
            }
        },
    )
}

pub(crate) fn parse_fetches<'a, T: Stream<Item = ResponseData> + Unpin>(
    stream: &'a mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> impl Stream<Item = Result<Fetch>> + 'a {
    use futures::StreamExt;

    StreamExt::filter_map(
        StreamExt::take_while(stream, move |res| match res.parsed() {
            Response::Done { tag, .. } => futures::future::ready(tag != &command_tag),
            _ => futures::future::ready(true),
        }),
        move |resp| {
            let unsolicited = unsolicited.clone();

            async move {
                match resp.parsed() {
                    Response::Fetch(..) => Some(Ok(Fetch::new(resp))),
                    _ => match handle_unilateral(&resp, unsolicited).await {
                        Some(resp) => match resp.parsed() {
                            Response::Fetch(..) => None,
                            resp => Some(Err(resp.into())),
                        },
                        None => None,
                    },
                }
            }
        },
    )
}

pub(crate) fn parse_expunge<'a, T: Stream<Item = ResponseData> + Unpin>(
    stream: &'a mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> impl Stream<Item = Result<u32>> + 'a {
    use futures::StreamExt;

    StreamExt::filter_map(
        StreamExt::take_while(stream, move |res| match res.parsed() {
            Response::Done { tag, .. } => futures::future::ready(&command_tag != tag),
            _ => futures::future::ready(true),
        }),
        move |resp| {
            let unsolicited = unsolicited.clone();

            async move {
                match resp.parsed() {
                    Response::Expunge(id) => Some(Ok(*id)),
                    _ => match handle_unilateral(&resp, unsolicited).await {
                        Some(resp) => match resp.parsed() {
                            Response::Fetch(..) => None,
                            resp => Some(Err(resp.into())),
                        },
                        None => None,
                    },
                }
            }
        },
    )
}

pub(crate) async fn parse_capabilities<'a, T: Stream<Item = ResponseData> + Unpin>(
    stream: &'a mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> Result<Capabilities> {
    let mut caps: HashSet<Capability> = HashSet::new();

    while let Some(resp) = stream
        .take_while(|res| match res.parsed() {
            Response::Done { tag, .. } => &command_tag != tag,
            _ => true,
        })
        .next()
        .await
    {
        match resp.parsed() {
            Response::Capabilities(cs) => {
                for c in cs {
                    caps.insert(Capability::from(c)); // TODO: avoid clone
                }
            }
            _ => {
                if let Some(resp) = handle_unilateral(&resp, unsolicited.clone()).await {
                    return Err(resp.parsed().into());
                }
            }
        }
    }

    Ok(Capabilities(caps))
}

pub(crate) async fn parse_noop<T: Stream<Item = ResponseData> + Unpin>(
    stream: &mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> Result<()> {
    let s = futures::StreamExt::filter_map(
        futures::StreamExt::take_while(stream, move |res| match res.parsed() {
            Response::Done { tag, .. } => futures::future::ready(&command_tag != tag),
            _ => futures::future::ready(true),
        }),
        move |resp| {
            let unsolicited = unsolicited.clone();

            async move {
                if let Some(resp) = handle_unilateral(&resp, unsolicited).await {
                    return Some(Err(resp.parsed().into()));
                }
                None
            }
        },
    );
    s.collect::<Result<()>>().await?;

    Ok(())
}

pub(crate) async fn parse_mailbox<T: Stream<Item = ResponseData> + Unpin>(
    stream: &mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> Result<Mailbox> {
    let mut mailbox = Mailbox::default();

    while let Some(resp) = stream
        .take_while(|res| match res.parsed() {
            Response::Done { tag, .. } => tag != &command_tag,
            _ => true,
        })
        .next()
        .await
    {
        println!("mailbox parsing {:?}", resp.parsed());
        match resp.parsed() {
            Response::Data { status, code, .. } => {
                if let imap_proto::Status::Ok = status {
                } else {
                    // how can this happen for a Response::Data?
                    unreachable!();
                }

                use imap_proto::ResponseCode;
                match code {
                    Some(ResponseCode::UidValidity(uid)) => {
                        mailbox.uid_validity = Some(*uid);
                    }
                    Some(ResponseCode::UidNext(unext)) => {
                        mailbox.uid_next = Some(*unext);
                    }
                    Some(ResponseCode::Unseen(n)) => {
                        mailbox.unseen = Some(*n);
                    }
                    Some(ResponseCode::PermanentFlags(flags)) => {
                        mailbox
                            .permanent_flags
                            .extend(flags.into_iter().map(|s| (*s).to_string()).map(Flag::from));
                    }
                    _ => {}
                }
            }
            Response::MailboxData(m) => match m {
                MailboxDatum::Status { mailbox, status } => {
                    unsolicited
                        .send(UnsolicitedResponse::Status {
                            mailbox: (*mailbox).into(),
                            attributes: status.to_vec(),
                        })
                        .await;
                }
                MailboxDatum::Exists(e) => {
                    mailbox.exists = *e;
                }
                MailboxDatum::Recent(r) => {
                    mailbox.recent = *r;
                }
                MailboxDatum::Flags(flags) => {
                    mailbox
                        .flags
                        .extend(flags.into_iter().map(|s| (*s).to_string()).map(Flag::from));
                }
                MailboxDatum::List { .. } => {}
                _ => {}
            },
            Response::Expunge(n) => {
                unsolicited.send(UnsolicitedResponse::Expunge(*n)).await;
            }
            _ => {
                return Err(resp.parsed().into());
            }
        }
    }

    println!("done mailbox parsing");
    Ok(mailbox)
}

pub(crate) async fn parse_ids<T: Stream<Item = ResponseData> + Unpin>(
    stream: &mut T,
    unsolicited: sync::Sender<UnsolicitedResponse>,
    command_tag: RequestId,
) -> Result<HashSet<u32>> {
    let mut ids: HashSet<u32> = HashSet::new();

    while let Some(resp) = stream
        .take_while(|res| match res.parsed() {
            Response::Done { tag, .. } => &command_tag != tag,
            _ => true,
        })
        .next()
        .await
    {
        match resp.parsed() {
            Response::IDs(cs) => {
                for c in cs {
                    ids.insert(*c);
                }
            }
            _ => {
                if let Some(resp) = handle_unilateral(&resp, unsolicited.clone()).await {
                    return Err(resp.parsed().into());
                }
            }
        }
    }

    Ok(ids)
}

// check if this is simply a unilateral server response
// (see Section 7 of RFC 3501):
async fn handle_unilateral<'a>(
    res: &'a ResponseData,
    unsolicited: sync::Sender<UnsolicitedResponse>,
) -> Option<&'a ResponseData> {
    match res.parsed() {
        Response::MailboxData(MailboxDatum::Status { mailbox, status }) => {
            unsolicited
                .send(UnsolicitedResponse::Status {
                    mailbox: (*mailbox).into(),
                    attributes: status.to_vec(),
                })
                .await;
        }
        Response::MailboxData(MailboxDatum::Recent(n)) => {
            unsolicited.send(UnsolicitedResponse::Recent(*n)).await;
        }
        Response::MailboxData(MailboxDatum::Exists(n)) => {
            unsolicited.send(UnsolicitedResponse::Exists(*n)).await;
        }
        Response::Expunge(n) => {
            unsolicited.send(UnsolicitedResponse::Expunge(*n)).await;
        }
        _res => {
            return Some(res);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_stream(data: &[&str]) -> Vec<ResponseData> {
        data.iter()
            .map(|line| match imap_proto::parse_response(line.as_bytes()) {
                Ok((remaining, response)) => {
                    let response = unsafe { std::mem::transmute(response) };
                    assert_eq!(remaining.len(), 0);

                    ResponseData {
                        raw: line.as_bytes().to_vec().into(),
                        response,
                    }
                }
                Err(err) => panic!("invalid input: {:?}", err),
            })
            .collect()
    }

    #[async_attributes::test]
    async fn parse_capability_test() {
        let expected_capabilities = vec!["IMAP4rev1", "STARTTLS", "AUTH=GSSAPI", "LOGINDISABLED"];
        let responses = input_stream(&vec![
            "* CAPABILITY IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n",
        ]);

        let mut stream = async_std::stream::from_iter(responses);
        let (send, recv) = sync::channel(10);
        let id = RequestId("A0001".into());
        let capabilities = parse_capabilities(&mut stream, send, id).await.unwrap();
        // shouldn't be any unexpected responses parsed
        assert!(recv.is_empty());
        assert_eq!(capabilities.len(), 4);
        for e in expected_capabilities {
            assert!(capabilities.has_str(e));
        }
    }

    #[async_attributes::test]
    async fn parse_capability_case_insensitive_test() {
        // Test that "IMAP4REV1" (instead of "IMAP4rev1") is accepted
        let expected_capabilities = vec!["IMAP4rev1", "STARTTLS"];
        let responses = input_stream(&vec!["* CAPABILITY IMAP4REV1 STARTTLS\r\n"]);
        let mut stream = async_std::stream::from_iter(responses);

        let (send, recv) = sync::channel(10);
        let id = RequestId("A0001".into());
        let capabilities = parse_capabilities(&mut stream, send, id).await.unwrap();

        // shouldn't be any unexpected responses parsed
        assert!(recv.is_empty());
        assert_eq!(capabilities.len(), 2);
        for e in expected_capabilities {
            assert!(capabilities.has_str(e));
        }
    }

    #[async_attributes::test]
    #[should_panic]
    async fn parse_capability_invalid_test() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
            "* JUNK IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n",
        ]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        parse_capabilities(&mut stream, send.clone(), id)
            .await
            .unwrap();
        assert!(recv.is_empty());
    }

    #[async_attributes::test]
    async fn parse_names_test() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec!["* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n"]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        let names: Vec<_> = parse_names(&mut stream, send, id)
            .collect::<Result<Vec<Name<'_>>>>()
            .await
            .unwrap();
        assert!(recv.is_empty());
        assert_eq!(names.len(), 1);
        assert_eq!(
            names[0].attributes(),
            &[NameAttribute::from("\\HasNoChildren")]
        );
        assert_eq!(names[0].delimiter(), Some("."));
        assert_eq!(names[0].name(), "INBOX");
    }

    #[async_attributes::test]
    async fn parse_fetches_empty() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![]);
        let mut stream = async_std::stream::from_iter(responses);
        let id = RequestId("a".into());

        let fetches = parse_fetches(&mut stream, send, id)
            .collect::<Result<Vec<_>>>()
            .await
            .unwrap();
        assert!(recv.is_empty());
        assert!(fetches.is_empty());
    }

    #[async_attributes::test]
    async fn parse_fetches_test() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
            "* 24 FETCH (FLAGS (\\Seen) UID 4827943)\r\n",
            "* 25 FETCH (FLAGS (\\Seen))\r\n",
        ]);
        let mut stream = async_std::stream::from_iter(responses);
        let id = RequestId("a".into());

        let fetches = parse_fetches(&mut stream, send, id)
            .collect::<Result<Vec<_>>>()
            .await
            .unwrap();
        assert!(recv.is_empty());

        assert_eq!(fetches.len(), 2);
        assert_eq!(fetches[0].message, 24);
        assert_eq!(fetches[0].flags().collect::<Vec<_>>(), vec![Flag::Seen]);
        assert_eq!(fetches[0].uid, Some(4827943));
        assert_eq!(fetches[0].body(), None);
        assert_eq!(fetches[0].header(), None);
        assert_eq!(fetches[1].message, 25);
        assert_eq!(fetches[1].flags().collect::<Vec<_>>(), vec![Flag::Seen]);
        assert_eq!(fetches[1].uid, None);
        assert_eq!(fetches[1].body(), None);
        assert_eq!(fetches[1].header(), None);
    }

    #[async_attributes::test]
    async fn parse_fetches_w_unilateral() {
        // https://github.com/mattnenterprise/rust-imap/issues/81
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec!["* 37 FETCH (UID 74)\r\n", "* 1 RECENT\r\n"]);
        let mut stream = async_std::stream::from_iter(responses);
        let id = RequestId("a".into());

        let fetches = parse_fetches(&mut stream, send, id)
            .collect::<Result<Vec<_>>>()
            .await
            .unwrap();
        assert_eq!(recv.recv().await, Some(UnsolicitedResponse::Recent(1)));

        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].message, 37);
        assert_eq!(fetches[0].uid, Some(74));
    }

    #[async_attributes::test]
    async fn parse_names_w_unilateral() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
            "* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n",
            "* 4 EXPUNGE\r\n",
        ]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        let names = parse_names(&mut stream, send, id)
            .collect::<Result<Vec<_>>>()
            .await
            .unwrap();

        assert_eq!(recv.recv().await, Some(UnsolicitedResponse::Expunge(4)));

        assert_eq!(names.len(), 1);
        assert_eq!(
            names[0].attributes(),
            &[NameAttribute::from("\\HasNoChildren")]
        );
        assert_eq!(names[0].delimiter(), Some("."));
        assert_eq!(names[0].name(), "INBOX");
    }

    #[async_attributes::test]
    async fn parse_capabilities_w_unilateral() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
            "* CAPABILITY IMAP4rev1 STARTTLS AUTH=GSSAPI LOGINDISABLED\r\n",
            "* STATUS dev.github (MESSAGES 10 UIDNEXT 11 UIDVALIDITY 1408806928 UNSEEN 0)\r\n",
            "* 4 EXISTS\r\n",
        ]);
        let mut stream = async_std::stream::from_iter(responses);

        let expected_capabilities = vec!["IMAP4rev1", "STARTTLS", "AUTH=GSSAPI", "LOGINDISABLED"];

        let id = RequestId("A0001".into());
        let capabilities = parse_capabilities(&mut stream, send, id).await.unwrap();

        assert_eq!(capabilities.len(), 4);
        for e in expected_capabilities {
            assert!(capabilities.has_str(e));
        }

        assert_eq!(
            recv.recv().await.unwrap(),
            UnsolicitedResponse::Status {
                mailbox: "dev.github".to_string(),
                attributes: vec![
                    StatusAttribute::Messages(10),
                    StatusAttribute::UidNext(11),
                    StatusAttribute::UidValidity(1408806928),
                    StatusAttribute::Unseen(0)
                ]
            }
        );
        assert_eq!(recv.recv().await.unwrap(), UnsolicitedResponse::Exists(4));
    }

    #[async_attributes::test]
    async fn parse_ids_w_unilateral() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
            "* SEARCH 23 42 4711\r\n",
            "* 1 RECENT\r\n",
            "* STATUS INBOX (MESSAGES 10 UIDNEXT 11 UIDVALIDITY 1408806928 UNSEEN 0)\r\n",
        ]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        let ids = parse_ids(&mut stream, send, id).await.unwrap();

        assert_eq!(ids, [23, 42, 4711].iter().cloned().collect());

        assert_eq!(recv.recv().await.unwrap(), UnsolicitedResponse::Recent(1));
        assert_eq!(
            recv.recv().await.unwrap(),
            UnsolicitedResponse::Status {
                mailbox: "INBOX".to_string(),
                attributes: vec![
                    StatusAttribute::Messages(10),
                    StatusAttribute::UidNext(11),
                    StatusAttribute::UidValidity(1408806928),
                    StatusAttribute::Unseen(0)
                ]
            }
        );
    }

    #[async_attributes::test]
    async fn parse_ids_test() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec![
                "* SEARCH 1600 1698 1739 1781 1795 1885 1891 1892 1893 1898 1899 1901 1911 1926 1932 1933 1993 1994 2007 2032 2033 2041 2053 2062 2063 2065 2066 2072 2078 2079 2082 2084 2095 2100 2101 2102 2103 2104 2107 2116 2120 2135 2138 2154 2163 2168 2172 2189 2193 2198 2199 2205 2212 2213 2221 2227 2267 2275 2276 2295 2300 2328 2330 2332 2333 2334\r\n",
                "* SEARCH 2335 2336 2337 2338 2339 2341 2342 2347 2349 2350 2358 2359 2362 2369 2371 2372 2373 2374 2375 2376 2377 2378 2379 2380 2381 2382 2383 2384 2385 2386 2390 2392 2397 2400 2401 2403 2405 2409 2411 2414 2417 2419 2420 2424 2426 2428 2439 2454 2456 2467 2468 2469 2490 2515 2519 2520 2521\r\n",
            ]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        let ids = parse_ids(&mut stream, send, id).await.unwrap();

        assert!(recv.is_empty());
        let ids: HashSet<u32> = ids.iter().cloned().collect();
        assert_eq!(
            ids,
            [
                1600, 1698, 1739, 1781, 1795, 1885, 1891, 1892, 1893, 1898, 1899, 1901, 1911, 1926,
                1932, 1933, 1993, 1994, 2007, 2032, 2033, 2041, 2053, 2062, 2063, 2065, 2066, 2072,
                2078, 2079, 2082, 2084, 2095, 2100, 2101, 2102, 2103, 2104, 2107, 2116, 2120, 2135,
                2138, 2154, 2163, 2168, 2172, 2189, 2193, 2198, 2199, 2205, 2212, 2213, 2221, 2227,
                2267, 2275, 2276, 2295, 2300, 2328, 2330, 2332, 2333, 2334, 2335, 2336, 2337, 2338,
                2339, 2341, 2342, 2347, 2349, 2350, 2358, 2359, 2362, 2369, 2371, 2372, 2373, 2374,
                2375, 2376, 2377, 2378, 2379, 2380, 2381, 2382, 2383, 2384, 2385, 2386, 2390, 2392,
                2397, 2400, 2401, 2403, 2405, 2409, 2411, 2414, 2417, 2419, 2420, 2424, 2426, 2428,
                2439, 2454, 2456, 2467, 2468, 2469, 2490, 2515, 2519, 2520, 2521
            ]
            .iter()
            .cloned()
            .collect()
        );
    }

    #[async_attributes::test]
    async fn parse_ids_search() {
        let (send, recv) = sync::channel(10);
        let responses = input_stream(&vec!["* SEARCH\r\n"]);
        let mut stream = async_std::stream::from_iter(responses);

        let id = RequestId("A0001".into());
        let ids = parse_ids(&mut stream, send, id).await.unwrap();

        assert!(recv.is_empty());
        let ids: HashSet<u32> = ids.iter().cloned().collect();
        assert_eq!(ids, HashSet::<u32>::new());
    }
}
