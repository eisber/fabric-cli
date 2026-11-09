// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Secret Service operations whose ordinary high-level wrappers execute Prompt.
//! Only an empty item marker uses the local authenticated D-Bus plain session.
//! Refresh credentials are subsequently written by secret-service's existing DH
//! session, never through this boundary. There is deliberately no Prompt API.

use super::InteractiveError;
use std::collections::HashMap;

pub(super) struct CreateReply {
    item: String,
    prompt: String,
}

pub(super) trait Boundary {
    fn create_empty(
        &self,
        collection: &str,
        attributes: HashMap<&str, &str>,
    ) -> Result<CreateReply, InteractiveError>;
    fn delete(&self, item: &str) -> Result<String, InteractiveError>;
}

pub(super) fn create_empty(
    boundary: &impl Boundary,
    collection: &str,
    attributes: HashMap<&str, &str>,
) -> Result<String, InteractiveError> {
    let reply = boundary.create_empty(collection, attributes)?;
    if reply.prompt != "/"
        || !reply
            .item
            .starts_with("/org/freedesktop/secrets/collection/")
    {
        return Err(InteractiveError::Store);
    }
    Ok(reply.item)
}

pub(super) fn delete(boundary: &impl Boundary, item: &str) -> Result<(), InteractiveError> {
    if boundary.delete(item)? != "/" {
        return Err(InteractiveError::Store);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) struct Native;

#[cfg(target_os = "linux")]
impl Boundary for Native {
    fn create_empty(
        &self,
        collection: &str,
        attributes: HashMap<&str, &str>,
    ) -> Result<CreateReply, InteractiveError> {
        use zbus::{
            blocking::{Connection, Proxy},
            zvariant::{Dict, OwnedObjectPath, OwnedValue, Value},
        };
        let connection = Connection::session().map_err(|_| InteractiveError::Store)?;
        let service = Proxy::new(
            &connection,
            "org.freedesktop.secrets",
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service",
        )
        .map_err(|_| InteractiveError::Store)?;
        // No credential bytes are ever sent through this plain session. Its
        // sole purpose is creating the empty item before the DH-only write.
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|_| InteractiveError::Store)?;
        let proxy = Proxy::new(
            &connection,
            "org.freedesktop.secrets",
            collection,
            "org.freedesktop.Secret.Collection",
        )
        .map_err(|_| InteractiveError::Store)?;
        let properties = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Value::from(super::SERVICE),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(Dict::from(attributes)),
            ),
        ]);
        let empty = (
            session,
            Vec::<u8>::new(),
            Vec::<u8>::new(),
            "application/octet-stream",
        );
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy
            .call("CreateItem", &(properties, empty, false))
            .map_err(|_| InteractiveError::Store)?;
        Ok(CreateReply {
            item: item.to_string(),
            prompt: prompt.to_string(),
        })
    }

    fn delete(&self, item: &str) -> Result<String, InteractiveError> {
        let connection =
            zbus::blocking::Connection::session().map_err(|_| InteractiveError::Store)?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.secrets",
            item,
            "org.freedesktop.Secret.Item",
        )
        .map_err(|_| InteractiveError::Store)?;
        let prompt: zbus::zvariant::OwnedObjectPath = proxy
            .call("Delete", &())
            .map_err(|_| InteractiveError::Store)?;
        Ok(prompt.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]
    use super::*;
    use std::sync::Mutex;

    struct Protocol {
        events: Mutex<Vec<&'static str>>,
        item: &'static str,
        prompt: &'static str,
    }

    impl Protocol {
        fn new(item: &'static str, prompt: &'static str) -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                item,
                prompt,
            }
        }
    }

    impl Boundary for Protocol {
        fn create_empty(
            &self,
            collection: &str,
            attributes: HashMap<&str, &str>,
        ) -> Result<CreateReply, InteractiveError> {
            assert_eq!(collection, "/org/freedesktop/secrets/collection/login");
            assert_eq!(attributes["service"], super::super::SERVICE);
            self.events.lock().unwrap().push("CreateItem");
            Ok(CreateReply {
                item: self.item.into(),
                prompt: self.prompt.into(),
            })
        }
        fn delete(&self, item: &str) -> Result<String, InteractiveError> {
            assert_eq!(item, "/org/freedesktop/secrets/collection/login/item");
            self.events.lock().unwrap().push("Delete");
            Ok(self.prompt.into())
        }
    }

    fn create(protocol: &Protocol) -> Result<String, InteractiveError> {
        create_empty(
            protocol,
            "/org/freedesktop/secrets/collection/login",
            HashMap::from([("service", super::super::SERVICE)]),
        )
    }

    #[test]
    fn prompt_required_create_never_reaches_a_credential_write_or_prompt() {
        for item in ["/", "/org/freedesktop/secrets/collection/login/item"] {
            let protocol = Protocol::new(item, "/org/freedesktop/secrets/prompt/1");
            assert_eq!(create(&protocol).unwrap_err(), InteractiveError::Store);
            assert_eq!(*protocol.events.lock().unwrap(), ["CreateItem"]);
        }
    }

    #[test]
    fn prompt_required_delete_fails_without_executing_or_waiting_for_prompt() {
        let protocol = Protocol::new("/", "/org/freedesktop/secrets/prompt/1");
        assert_eq!(
            delete(&protocol, "/org/freedesktop/secrets/collection/login/item").unwrap_err(),
            InteractiveError::Store
        );
        assert_eq!(*protocol.events.lock().unwrap(), ["Delete"]);
    }

    #[test]
    fn only_immediate_create_and_delete_results_are_accepted() {
        let protocol = Protocol::new("/org/freedesktop/secrets/collection/login/item", "/");
        assert_eq!(create(&protocol).unwrap(), protocol.item);
        delete(&protocol, protocol.item).unwrap();
        assert_eq!(*protocol.events.lock().unwrap(), ["CreateItem", "Delete"]);
        let absent = Protocol::new("/", "/");
        assert_eq!(create(&absent).unwrap_err(), InteractiveError::Store);
    }
}
