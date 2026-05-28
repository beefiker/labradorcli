// Our class for handling NSServices messages.
@interface LabradorServicesProvider : NSObject
@end

// Functions implemented in Rust.
id labrador_services_provider_custom_url_scheme();
void labrador_app_open_urls(id app, id urls);
