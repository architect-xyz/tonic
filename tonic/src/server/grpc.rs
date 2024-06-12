use crate::codec::compression::{
    CompressionEncoding, EnabledCompressionEncodings, SingleMessageCompressionOverride,
};
use crate::{
    body::BoxBody,
    codec::{encode_server, Codec, Streaming},
    server::{ClientStreamingService, ServerStreamingService, StreamingService, UnaryService},
    Code, Request, Status,
};
use http_body::Body;
use std::{fmt, pin::pin};
use tokio_stream::{Stream, StreamExt};

macro_rules! t {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(status) => return status.to_http(),
        }
    };
}

/// A gRPC Server handler.
///
/// This will wrap some inner [`Codec`] and provide utilities to handle
/// inbound unary, client side streaming, server side streaming, and
/// bi-directional streaming.
///
/// Each request handler method accepts some service that implements the
/// corresponding service trait and a http request that contains some body that
/// implements some [`Body`].
pub struct Grpc<T> {
    codec: T,
    /// Which compression encodings does the server accept for requests?
    accept_compression_encodings: EnabledCompressionEncodings,
    /// Which compression encodings might the server use for responses.
    send_compression_encodings: EnabledCompressionEncodings,
    /// Limits the maximum size of a decoded message.
    max_decoding_message_size: Option<usize>,
    /// Limits the maximum size of an encoded message.
    max_encoding_message_size: Option<usize>,
}

impl<T> Grpc<T>
where
    T: Codec,
{
    /// Creates a new gRPC server with the provided [`Codec`].
    pub fn new(codec: T) -> Self {
        Self {
            codec,
            accept_compression_encodings: EnabledCompressionEncodings::default(),
            send_compression_encodings: EnabledCompressionEncodings::default(),
            max_decoding_message_size: None,
            max_encoding_message_size: None,
        }
    }

    /// Enable accepting compressed requests.
    ///
    /// If a request with an unsupported encoding is received the server will respond with
    /// [`Code::UnUnimplemented`](crate::Code).
    ///
    /// # Example
    ///
    /// The most common way of using this is through a server generated by tonic-build:
    ///
    /// ```rust
    /// # enum CompressionEncoding { Gzip }
    /// # struct Svc;
    /// # struct ExampleServer<T>(T);
    /// # impl<T> ExampleServer<T> {
    /// #     fn new(svc: T) -> Self { Self(svc) }
    /// #     fn accept_compressed(self, _: CompressionEncoding) -> Self { self }
    /// # }
    /// # #[tonic::async_trait]
    /// # trait Example {}
    ///
    /// #[tonic::async_trait]
    /// impl Example for Svc {
    ///     // ...
    /// }
    ///
    /// let service = ExampleServer::new(Svc).accept_compressed(CompressionEncoding::Gzip);
    /// ```
    pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
        self.accept_compression_encodings.enable(encoding);
        self
    }

    /// Enable sending compressed responses.
    ///
    /// Requires the client to also support receiving compressed responses.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a server generated by tonic-build:
    ///
    /// ```rust
    /// # enum CompressionEncoding { Gzip }
    /// # struct Svc;
    /// # struct ExampleServer<T>(T);
    /// # impl<T> ExampleServer<T> {
    /// #     fn new(svc: T) -> Self { Self(svc) }
    /// #     fn send_compressed(self, _: CompressionEncoding) -> Self { self }
    /// # }
    /// # #[tonic::async_trait]
    /// # trait Example {}
    ///
    /// #[tonic::async_trait]
    /// impl Example for Svc {
    ///     // ...
    /// }
    ///
    /// let service = ExampleServer::new(Svc).send_compressed(CompressionEncoding::Gzip);
    /// ```
    pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
        self.send_compression_encodings.enable(encoding);
        self
    }

    /// Limits the maximum size of a decoded message.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a server generated by tonic-build:
    ///
    /// ```rust
    /// # struct Svc;
    /// # struct ExampleServer<T>(T);
    /// # impl<T> ExampleServer<T> {
    /// #     fn new(svc: T) -> Self { Self(svc) }
    /// #     fn max_decoding_message_size(self, _: usize) -> Self { self }
    /// # }
    /// # #[tonic::async_trait]
    /// # trait Example {}
    ///
    /// #[tonic::async_trait]
    /// impl Example for Svc {
    ///     // ...
    /// }
    ///
    /// // Set the limit to 2MB, Defaults to 4MB.
    /// let limit = 2 * 1024 * 1024;
    /// let service = ExampleServer::new(Svc).max_decoding_message_size(limit);
    /// ```
    pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
        self.max_decoding_message_size = Some(limit);
        self
    }

    /// Limits the maximum size of a encoded message.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a server generated by tonic-build:
    ///
    /// ```rust
    /// # struct Svc;
    /// # struct ExampleServer<T>(T);
    /// # impl<T> ExampleServer<T> {
    /// #     fn new(svc: T) -> Self { Self(svc) }
    /// #     fn max_encoding_message_size(self, _: usize) -> Self { self }
    /// # }
    /// # #[tonic::async_trait]
    /// # trait Example {}
    ///
    /// #[tonic::async_trait]
    /// impl Example for Svc {
    ///     // ...
    /// }
    ///
    /// // Set the limit to 2MB, Defaults to 4MB.
    /// let limit = 2 * 1024 * 1024;
    /// let service = ExampleServer::new(Svc).max_encoding_message_size(limit);
    /// ```
    pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
        self.max_encoding_message_size = Some(limit);
        self
    }

    #[doc(hidden)]
    pub fn apply_compression_config(
        self,
        accept_encodings: EnabledCompressionEncodings,
        send_encodings: EnabledCompressionEncodings,
    ) -> Self {
        let mut this = self;

        for &encoding in CompressionEncoding::encodings() {
            if accept_encodings.is_enabled(encoding) {
                this = this.accept_compressed(encoding);
            }
            if send_encodings.is_enabled(encoding) {
                this = this.send_compressed(encoding);
            }
        }

        this
    }

    #[doc(hidden)]
    pub fn apply_max_message_size_config(
        self,
        max_decoding_message_size: Option<usize>,
        max_encoding_message_size: Option<usize>,
    ) -> Self {
        let mut this = self;

        if let Some(limit) = max_decoding_message_size {
            this = this.max_decoding_message_size(limit);
        }
        if let Some(limit) = max_encoding_message_size {
            this = this.max_encoding_message_size(limit);
        }

        this
    }

    /// Handle a single unary gRPC request.
    pub async fn unary<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: UnaryService<T::Decode, Response = T::Encode>,
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let accept_encoding = CompressionEncoding::from_accept_encoding_header(
            req.headers(),
            self.send_compression_encodings,
        );

        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self.map_response::<tokio_stream::Once<Result<T::Encode, Status>>>(
                    Err(status),
                    accept_encoding,
                    SingleMessageCompressionOverride::default(),
                    self.max_encoding_message_size,
                );
            }
        };

        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| tokio_stream::once(Ok(m))));

        let compression_override = compression_override_from_response(&response);

        self.map_response(
            response,
            accept_encoding,
            compression_override,
            self.max_encoding_message_size,
        )
    }

    /// Handle a server side streaming request.
    pub async fn server_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ServerStreamingService<T::Decode, Response = T::Encode>,
        S::ResponseStream: Send + 'static,
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let accept_encoding = CompressionEncoding::from_accept_encoding_header(
            req.headers(),
            self.send_compression_encodings,
        );

        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self.map_response::<S::ResponseStream>(
                    Err(status),
                    accept_encoding,
                    SingleMessageCompressionOverride::default(),
                    self.max_encoding_message_size,
                );
            }
        };

        let response = service.call(request).await;

        self.map_response(
            response,
            accept_encoding,
            // disabling compression of individual stream items must be done on
            // the items themselves
            SingleMessageCompressionOverride::default(),
            self.max_encoding_message_size,
        )
    }

    /// Handle a client side streaming gRPC request.
    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ClientStreamingService<T::Decode, Response = T::Encode>,
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send + 'static,
    {
        let accept_encoding = CompressionEncoding::from_accept_encoding_header(
            req.headers(),
            self.send_compression_encodings,
        );

        let request = t!(self.map_request_streaming(req));

        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| tokio_stream::once(Ok(m))));

        let compression_override = compression_override_from_response(&response);

        self.map_response(
            response,
            accept_encoding,
            compression_override,
            self.max_encoding_message_size,
        )
    }

    /// Handle a bi-directional streaming gRPC request.
    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: StreamingService<T::Decode, Response = T::Encode> + Send,
        S::ResponseStream: Send + 'static,
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let accept_encoding = CompressionEncoding::from_accept_encoding_header(
            req.headers(),
            self.send_compression_encodings,
        );

        let request = t!(self.map_request_streaming(req));

        let response = service.call(request).await;

        self.map_response(
            response,
            accept_encoding,
            SingleMessageCompressionOverride::default(),
            self.max_encoding_message_size,
        )
    }

    async fn map_request_unary<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Result<Request<T::Decode>, Status>
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let request_compression_encoding = self.request_encoding_if_supported(&request)?;

        let (parts, body) = request.into_parts();

        let mut stream = pin!(Streaming::new_request(
            self.codec.decoder(),
            body,
            request_compression_encoding,
            self.max_decoding_message_size,
        ));

        let message = stream
            .try_next()
            .await?
            .ok_or_else(|| Status::new(Code::Internal, "Missing request message."))?;

        let mut req = Request::from_http_parts(parts, message);

        if let Some(trailers) = stream.trailers().await? {
            req.metadata_mut().merge(trailers);
        }

        Ok(req)
    }

    fn map_request_streaming<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Result<Request<Streaming<T::Decode>>, Status>
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let encoding = self.request_encoding_if_supported(&request)?;

        let request = request.map(|body| {
            Streaming::new_request(
                self.codec.decoder(),
                body,
                encoding,
                self.max_decoding_message_size,
            )
        });

        Ok(Request::from_http(request))
    }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
        accept_encoding: Option<CompressionEncoding>,
        compression_override: SingleMessageCompressionOverride,
        max_message_size: Option<usize>,
    ) -> http::Response<BoxBody>
    where
        B: Stream<Item = Result<T::Encode, Status>> + Send + 'static,
    {
        let response = t!(response);

        let (mut parts, body) = response.into_http().into_parts();

        // Set the content type
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_static("application/grpc"),
        );

        #[cfg(any(feature = "gzip", feature = "zstd"))]
        if let Some(encoding) = accept_encoding {
            // Set the content encoding
            parts.headers.insert(
                crate::codec::compression::ENCODING_HEADER,
                encoding.into_header_value(),
            );
        }

        let body = encode_server(
            self.codec.encoder(),
            body,
            accept_encoding,
            compression_override,
            max_message_size,
        );

        http::Response::from_parts(parts, BoxBody::new(body))
    }

    fn request_encoding_if_supported<B>(
        &self,
        request: &http::Request<B>,
    ) -> Result<Option<CompressionEncoding>, Status> {
        CompressionEncoding::from_encoding_header(
            request.headers(),
            self.accept_compression_encodings,
        )
    }
}

impl<T: fmt::Debug> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Grpc");

        f.field("codec", &self.codec);

        f.field(
            "accept_compression_encodings",
            &self.accept_compression_encodings,
        );

        f.field(
            "send_compression_encodings",
            &self.send_compression_encodings,
        );

        f.finish()
    }
}

fn compression_override_from_response<B, E>(
    res: &Result<crate::Response<B>, E>,
) -> SingleMessageCompressionOverride {
    res.as_ref()
        .ok()
        .and_then(|response| {
            response
                .extensions()
                .get::<SingleMessageCompressionOverride>()
                .copied()
        })
        .unwrap_or_default()
}
