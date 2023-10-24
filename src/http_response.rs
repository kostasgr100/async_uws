use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::unbounded_channel;
use uwebsockets_rs::http_request::HttpRequest;
use uwebsockets_rs::http_response::HttpResponseStruct;
use uwebsockets_rs::uws_loop::{loop_defer, UwsLoop};
use uwebsockets_rs::websocket_behavior::UpgradeContext;

use crate::data_storage::SharedDataStorage;
use crate::ws_behavior::{WsPerSocketUserData, WsPerSocketUserDataStorage};
use crate::ws_message::WsMessage;

pub struct HttpResponse<const SSL: bool> {
    pub(crate) native: Option<HttpResponseStruct<SSL>>,
    pub(crate) uws_loop: UwsLoop,
    pub is_aborted: Arc<AtomicBool>,
    data_storage: SharedDataStorage,
    per_socket_data_storage: Option<WsPerSocketUserDataStorage>,
    upgrade_context: Option<UpgradeContext>,
}

unsafe impl<const SSL: bool> Sync for HttpResponse<SSL> {}
unsafe impl<const SSL: bool> Send for HttpResponse<SSL> {}

impl<const SSL: bool> HttpResponse<SSL> {
    pub fn new(
        native_response: HttpResponseStruct<SSL>,
        uws_loop: UwsLoop,
        is_aborted: Arc<AtomicBool>,
        data_storage: SharedDataStorage,
        // Will be not None only for upgrade requests
        per_socket_data_storage: Option<WsPerSocketUserDataStorage>,
        // Will be not None only for upgrade requests
        upgrade_context: Option<UpgradeContext>,
    ) -> Self {
        HttpResponse {
            native: Some(native_response),
            is_aborted,
            uws_loop,
            data_storage,
            per_socket_data_storage,
            upgrade_context,
        }
    }

    pub fn data<T: Send + Sync + Clone + 'static>(&self) -> Option<&T> {
        self.data_storage.as_ref().get_data::<T>()
    }

    pub fn end(mut self, data: Option<&'static str>, close_connection: bool) {
        tokio::spawn(async move {
            let uws_loop = self.uws_loop;

            let callback = move || {
                let res = self.native.take().unwrap();
                res.end(data, close_connection);
            };

            loop_defer(uws_loop, callback);
        });
    }

    pub fn write_status(&self, status: &str) {
        if let Some(response) = self.native.as_ref() {
            response.write_status(status);
        }
    }

    pub fn write_header(&self, key: &str, value: &str) {
        if let Some(response) = self.native.as_ref() {
            response.write_header(key, value);
        }
    }

    pub fn write_header_int(&self, key: &str, value: u64) {
        if let Some(response) = self.native.as_ref() {
            response.write_header_int(key, value);
        }
    }

    pub fn end_without_body(&self, close_connection: bool) {
        if let Some(response) = self.native.as_ref() {
            response.end_without_body(close_connection);
        }
    }

    pub fn has_responded(&self) -> bool {
        if let Some(response) = self.native.as_ref() {
            response.has_responded()
        } else {
            true
        }
    }

    pub fn upgrade(
        self,
        ws_key_string: String,
        ws_protocol: Option<String>,
        ws_extensions: Option<String>,
        user_data: Option<SharedDataStorage>,
    ) {
        let (sink, stream) = unbounded_channel::<WsMessage>();
        let user_data_id = ws_key_string.to_owned();

        let ws_per_socket_data_storage = self.per_socket_data_storage.clone().unwrap();
        let user_data = WsPerSocketUserData {
            sink,
            id: user_data_id.to_owned(),
            stream: Some(stream),
            storage: ws_per_socket_data_storage.clone(),
            is_open: Arc::new(AtomicBool::new(true)),
            shared_data_storage: self.data_storage.clone(),
            custom_user_data: user_data.unwrap_or_default(),
        };

        {
            let mut storage = ws_per_socket_data_storage.lock().unwrap();
            storage.insert(user_data_id.to_owned(), Box::new(user_data));
        }

        let is_aborted = self.is_aborted.load(Ordering::SeqCst);
        if is_aborted {
            println!("[async_uws] Upgrade request is aborted");
            return;
        }

        let storage_to_move = ws_per_socket_data_storage.clone();

        let callback = move || {
            let mut storage = storage_to_move.lock().unwrap();
            let user_data_ref = storage.get_mut(&user_data_id).unwrap().as_mut();

            let ws_protocol: Option<&str> = ws_protocol.as_deref();
            let ws_extensions: Option<&str> = ws_extensions.as_deref();

            self.native.unwrap().upgrade(
                &ws_key_string,
                ws_protocol,
                ws_extensions,
                self.upgrade_context.unwrap(),
                Some(user_data_ref),
            );
        };

        loop_defer(self.uws_loop, callback)
    }

    pub fn default_upgrade(req: HttpRequest, res: HttpResponse<SSL>) {
        let ws_key_string = req
            .get_header("sec-websocket-key")
            .expect("[async_uws]: There is no sec-websocket-key in req headers")
            .to_string();
        let ws_protocol = req.get_header("sec-websocket-protocol").map(String::from);
        let ws_extensions =
            req.get_header("sec-websocket-extensions").map(String::from);

        res.upgrade(ws_key_string, ws_protocol, ws_extensions, None);
    }
}
