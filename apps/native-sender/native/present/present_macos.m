#import <AppKit/AppKit.h>
#import <CoreVideo/CoreVideo.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#include <string.h>

static NSString *const kShader = @
    "#include <metal_stdlib>\n"
    "using namespace metal;\n"
    "struct VOut { float4 pos [[position]]; float2 uv; };\n"
    "vertex VOut vs_main(uint id [[vertex_id]], constant float *fit [[buffer(0)]]) {\n"
    "  float2 corners[6] = {\n"
    "    float2(-1.0, -1.0), float2(1.0, -1.0), float2(-1.0, 1.0),\n"
    "    float2(-1.0, 1.0), float2(1.0, -1.0), float2(1.0, 1.0)\n"
    "  };\n"
    "  float2 ndc = corners[id] * float2(fit[0], fit[1]);\n"
    "  VOut out;\n"
    "  out.pos = float4(ndc, 0.0, 1.0);\n"
    "  out.uv = float2((corners[id].x + 1.0) * 0.5, (1.0 - corners[id].y) * 0.5);\n"
    "  return out;\n"
    "}\n"
    "float3 paint_cursor(float3 rgb, float2 uv, constant float *cursor) {\n"
    "  if (cursor[2] < 0.5) return rgb;\n"
    "  float2 px = (uv - float2(cursor[0], cursor[1])) * float2(cursor[3], cursor[4]);\n"
    "  float x = px.x;\n"
    "  float y = px.y;\n"
    "  bool arrow = x >= 0.0 && y >= 0.0 && x < 12.0 && y < 16.0 && (x + y * 0.55) < 12.0;\n"
    "  if (!arrow) return rgb;\n"
    "  bool edge = x < 1.0 || y < 1.0 || (x + y * 0.55) > 10.6;\n"
    "  return edge ? float3(0.0) : float3(1.0);\n"
    "}\n"
    "fragment float4 fs_nv12(VOut in [[stage_in]],\n"
    "                        texture2d<float> yTex [[texture(0)]],\n"
    "                        texture2d<float> uvTex [[texture(1)]],\n"
    "                        constant uint *range [[buffer(0)]],\n"
    "                        constant float *cursor [[buffer(1)]]) {\n"
    "  constexpr sampler s(address::clamp_to_edge, filter::linear);\n"
    "  float y = yTex.sample(s, in.uv).r;\n"
    "  float2 uv = uvTex.sample(s, in.uv).rg - 0.5;\n"
    "  float3 rgb;\n"
    "  if (range[0] != 0) {\n"
    "    rgb = float3(y + 1.402 * uv.g,\n"
    "                 y - 0.344136 * uv.r - 0.714136 * uv.g,\n"
    "                 y + 1.772 * uv.r);\n"
    "  } else {\n"
    "    float c = y - (16.0 / 255.0);\n"
    "    rgb = float3(1.164383 * c + 1.596027 * uv.g,\n"
    "                 1.164383 * c - 0.391762 * uv.r - 0.812968 * uv.g,\n"
    "                 1.164383 * c + 2.017232 * uv.r);\n"
    "  }\n"
    "  return float4(saturate(paint_cursor(rgb, in.uv, cursor)), 1.0);\n"
    "}\n"
    "fragment float4 fs_bgra(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]],\n"
    "                        constant float *cursor [[buffer(0)]]) {\n"
    "  constexpr sampler s(address::clamp_to_edge, filter::linear);\n"
    "  float3 rgb = paint_cursor(tex.sample(s, in.uv).rgb, in.uv, cursor);\n"
    "  return float4(rgb, 1.0);\n"
    "}\n";

@interface DcmaxPresenter : NSObject
@property(nonatomic, strong) id<MTLDevice> device;
@property(nonatomic, strong) id<MTLCommandQueue> commandQueue;
@property(nonatomic, strong) id<MTLLibrary> library;
@property(nonatomic, strong) id<MTLRenderPipelineState> nv12Pipeline;
@property(nonatomic, strong) id<MTLRenderPipelineState> bgraPipeline;
@property(nonatomic, strong) CAMetalLayer *metalLayer;
@property(nonatomic, strong) NSView *view;
@property(nonatomic, assign) CVMetalTextureCacheRef cache;
@property(nonatomic, strong) id<MTLTexture> bgraTexture;
@property(nonatomic, assign) NSUInteger bgraWidth;
@property(nonatomic, assign) NSUInteger bgraHeight;
@end

@implementation DcmaxPresenter

- (void)dealloc {
    if (_cache) {
        CFRelease(_cache);
        _cache = NULL;
    }
}

- (id<MTLRenderPipelineState>)pipelineNamed:(NSString *)fragment error:(NSError **)error {
    id<MTLFunction> vertex = [self.library newFunctionWithName:@"vs_main"];
    id<MTLFunction> frag = [self.library newFunctionWithName:fragment];
    if (!vertex || !frag) {
        if (error) {
            *error = [NSError errorWithDomain:@"dcmax"
                                         code:1
                                     userInfo:@{NSLocalizedDescriptionKey : @"shader function missing"}];
        }
        return nil;
    }
    MTLRenderPipelineDescriptor *desc = [MTLRenderPipelineDescriptor new];
    desc.vertexFunction = vertex;
    desc.fragmentFunction = frag;
    desc.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
    return [self.device newRenderPipelineStateWithDescriptor:desc error:error];
}

- (BOOL)fitContentWidth:(size_t)width height:(size_t)height into:(float *)fit {
    [self syncLayerSize];
    CGSize drawable = self.metalLayer.drawableSize;
    if (drawable.width < 2 || drawable.height < 2 || width == 0 || height == 0) {
        return NO;
    }
    float contentAspect = (float)width / (float)height;
    float viewAspect = (float)drawable.width / (float)drawable.height;
    fit[0] = 1.0f;
    fit[1] = 1.0f;
    if (viewAspect > contentAspect) {
        fit[0] = contentAspect / viewAspect;
    } else if (contentAspect > 0.0f) {
        fit[1] = viewAspect / contentAspect;
    }
    return YES;
}

- (void)syncLayerSize {
    if (self.view.layer != self.metalLayer) {
        self.view.wantsLayer = YES;
        self.view.layer = self.metalLayer;
    }
    CGFloat scale = self.view.window.backingScaleFactor;
    if (scale < 1.0) {
        scale = NSScreen.mainScreen.backingScaleFactor;
    }
    if (scale < 1.0) {
        scale = 2.0;
    }
    NSSize size = self.view.bounds.size;
    if ((size.width < 2.0 || size.height < 2.0) && self.view.window) {
        size = self.view.window.contentLayoutRect.size;
    }
    self.metalLayer.contentsScale = scale;
    self.metalLayer.frame = CGRectMake(0.0, 0.0, MAX(size.width, 1.0), MAX(size.height, 1.0));
    self.metalLayer.drawableSize = CGSizeMake(MAX(size.width * scale, 2.0), MAX(size.height * scale, 2.0));
}

@end

static void copy_error(char *error, int cap, NSString *text) {
    if (!error || cap <= 0) {
        return;
    }
    const char *utf = text.UTF8String ?: "metal presenter failed";
    strlcpy(error, utf, (size_t)cap);
}

void *dcmax_presenter_new(void *ns_view, char *error, int error_cap) {
    @autoreleasepool {
        if (!ns_view) {
            copy_error(error, error_cap, @"missing view");
            return NULL;
        }
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) {
            copy_error(error, error_cap, @"this Mac has no Metal device");
            return NULL;
        }
        NSError *shaderError = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:kShader options:nil error:&shaderError];
        if (!library) {
            copy_error(error, error_cap, shaderError.localizedDescription ?: @"shader compile failed");
            return NULL;
        }
        DcmaxPresenter *presenter = [DcmaxPresenter new];
        presenter.device = device;
        presenter.library = library;
        presenter.commandQueue = [device newCommandQueue];
        presenter.view = (__bridge NSView *)ns_view;
        NSError *pipelineError = nil;
        presenter.nv12Pipeline = [presenter pipelineNamed:@"fs_nv12" error:&pipelineError];
        if (!presenter.nv12Pipeline) {
            copy_error(error, error_cap, pipelineError.localizedDescription ?: @"nv12 pipeline failed");
            return NULL;
        }
        presenter.bgraPipeline = [presenter pipelineNamed:@"fs_bgra" error:&pipelineError];
        if (!presenter.bgraPipeline) {
            copy_error(error, error_cap, pipelineError.localizedDescription ?: @"bgra pipeline failed");
            return NULL;
        }
        CVMetalTextureCacheRef cache = NULL;
        CVReturn created = CVMetalTextureCacheCreate(NULL, NULL, device, NULL, &cache);
        if (created != kCVReturnSuccess || !cache) {
            copy_error(error, error_cap, @"could not create the Metal texture cache");
            return NULL;
        }
        presenter.cache = cache;
        CAMetalLayer *layer = [CAMetalLayer layer];
        layer.device = device;
        layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
        layer.framebufferOnly = YES;
        layer.opaque = YES;
        presenter.view.wantsLayer = YES;
        presenter.view.layerContentsRedrawPolicy = NSViewLayerContentsRedrawNever;
        presenter.view.layer = layer;
        presenter.metalLayer = layer;
        [presenter syncLayerSize];
        if (error && error_cap > 0) {
            error[0] = 0;
        }
        return (__bridge_retained void *)presenter;
    }
}

void dcmax_presenter_resize(void *raw) {
    if (!raw) {
        return;
    }
    DcmaxPresenter *presenter = (__bridge DcmaxPresenter *)raw;
    [presenter syncLayerSize];
}

static int draw_textures(DcmaxPresenter *presenter, float *fit, id<MTLTexture> y, id<MTLTexture> uv, int full_range, const float *cursor) {
    if (!y || !uv) {
        return -3;
    }
    uint32_t range = full_range ? 1u : 0u;
    id<CAMetalDrawable> drawable = [presenter.metalLayer nextDrawable];
    if (!drawable) {
        return -2;
    }
    id<MTLCommandBuffer> buffer = [presenter.commandQueue commandBuffer];
    MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
    pass.colorAttachments[0].texture = drawable.texture;
    pass.colorAttachments[0].loadAction = MTLLoadActionClear;
    pass.colorAttachments[0].storeAction = MTLStoreActionStore;
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0.0, 0.0, 0.0, 1.0);
    id<MTLRenderCommandEncoder> encoder = [buffer renderCommandEncoderWithDescriptor:pass];
    [encoder setRenderPipelineState:presenter.nv12Pipeline];
    [encoder setVertexBytes:fit length:sizeof(float) * 2 atIndex:0];
    [encoder setFragmentTexture:y atIndex:0];
    [encoder setFragmentTexture:uv atIndex:1];
    [encoder setFragmentBytes:&range length:sizeof(range) atIndex:0];
    [encoder setFragmentBytes:cursor length:sizeof(float) * 5 atIndex:1];
    [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:6];
    [encoder endEncoding];
    [buffer presentDrawable:drawable];
    [buffer commit];
    return 0;
}

int dcmax_presenter_draw_nv12(void *raw, void *pixel_buffer, int full_range, const float *cursor) {
    if (!raw || !pixel_buffer) {
        return -1;
    }
    @autoreleasepool {
        DcmaxPresenter *presenter = (__bridge DcmaxPresenter *)raw;
        CVPixelBufferRef pixel = (CVPixelBufferRef)pixel_buffer;
        if (!CVPixelBufferIsPlanar(pixel) || CVPixelBufferGetPlaneCount(pixel) < 2) {
            return -3;
        }
        size_t width = CVPixelBufferGetWidthOfPlane(pixel, 0);
        size_t height = CVPixelBufferGetHeightOfPlane(pixel, 0);
        size_t chromaWidth = CVPixelBufferGetWidthOfPlane(pixel, 1);
        size_t chromaHeight = CVPixelBufferGetHeightOfPlane(pixel, 1);
        float fit[2] = {1.0f, 1.0f};
        if (![presenter fitContentWidth:width height:height into:fit]) {
            return -2;
        }
        CVMetalTextureRef yRef = NULL;
        CVMetalTextureRef uvRef = NULL;
        CVReturn yStatus = CVMetalTextureCacheCreateTextureFromImage(
            NULL, presenter.cache, pixel, NULL, MTLPixelFormatR8Unorm, width, height, 0, &yRef);
        CVReturn uvStatus = CVMetalTextureCacheCreateTextureFromImage(
            NULL, presenter.cache, pixel, NULL, MTLPixelFormatRG8Unorm, chromaWidth, chromaHeight, 1, &uvRef);
        if (yStatus != kCVReturnSuccess || uvStatus != kCVReturnSuccess || !yRef || !uvRef) {
            if (yRef) CFRelease(yRef);
            if (uvRef) CFRelease(uvRef);
            return -3;
        }
        id<MTLTexture> y = CVMetalTextureGetTexture(yRef);
        id<MTLTexture> uv = CVMetalTextureGetTexture(uvRef);
        float pointer[5] = {0.5f, 0.5f, 0.0f, (float)width, (float)height};
        if (cursor) {
            memcpy(pointer, cursor, sizeof(pointer));
            pointer[3] = (float)width;
            pointer[4] = (float)height;
        }
        int drawn = draw_textures(presenter, fit, y, uv, full_range, pointer);
        CFRelease(yRef);
        CFRelease(uvRef);
        CVMetalTextureCacheFlush(presenter.cache, 0);
        return drawn;
    }
}

int dcmax_presenter_draw_bgra(void *raw, const void *bytes, uint32_t width, uint32_t height, uint32_t stride, const float *cursor) {
    if (!raw || !bytes || width == 0 || height == 0 || stride < width * 4) {
        return -1;
    }
    @autoreleasepool {
        DcmaxPresenter *presenter = (__bridge DcmaxPresenter *)raw;
        float fit[2] = {1.0f, 1.0f};
        if (![presenter fitContentWidth:width height:height into:fit]) {
            return -2;
        }
        if (presenter.bgraTexture == nil || presenter.bgraWidth != width || presenter.bgraHeight != height) {
            MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                            width:width
                                                                                           height:height
                                                                                        mipmapped:NO];
            desc.usage = MTLTextureUsageShaderRead;
            presenter.bgraTexture = [presenter.device newTextureWithDescriptor:desc];
            presenter.bgraWidth = width;
            presenter.bgraHeight = height;
        }
        if (!presenter.bgraTexture) {
            return -4;
        }
        MTLRegion region = MTLRegionMake2D(0, 0, width, height);
        [presenter.bgraTexture replaceRegion:region mipmapLevel:0 withBytes:bytes bytesPerRow:stride];
        id<CAMetalDrawable> drawable = [presenter.metalLayer nextDrawable];
        if (!drawable) {
            return -2;
        }
        id<MTLCommandBuffer> buffer = [presenter.commandQueue commandBuffer];
        MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = drawable.texture;
        pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore;
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0.0, 0.0, 0.0, 1.0);
        id<MTLRenderCommandEncoder> encoder = [buffer renderCommandEncoderWithDescriptor:pass];
        [encoder setRenderPipelineState:presenter.bgraPipeline];
        [encoder setVertexBytes:fit length:sizeof(fit) atIndex:0];
        [encoder setFragmentTexture:presenter.bgraTexture atIndex:0];
        float pointer[5] = {0.5f, 0.5f, 0.0f, (float)width, (float)height};
        if (cursor) {
            memcpy(pointer, cursor, sizeof(float) * 3);
            pointer[3] = (float)width;
            pointer[4] = (float)height;
        }
        [encoder setFragmentBytes:pointer length:sizeof(pointer) atIndex:0];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:6];
        [encoder endEncoding];
        [buffer presentDrawable:drawable];
        [buffer commit];
        return 0;
    }
}

void dcmax_presenter_free(void *raw) {
    if (!raw) {
        return;
    }
    DcmaxPresenter *presenter = (__bridge_transfer DcmaxPresenter *)raw;
    presenter.view.layer = nil;
}

typedef void (*DcmaxVsyncFn)(void *);

static CVDisplayLinkRef g_link = NULL;
static DcmaxVsyncFn g_vsync = NULL;

static CVReturn dcmax_vsync_callback(CVDisplayLinkRef link,
                                     const CVTimeStamp *now,
                                     const CVTimeStamp *output,
                                     CVOptionFlags flags_in,
                                     CVOptionFlags *flags_out,
                                     void *context) {
    (void)link;
    (void)now;
    (void)output;
    (void)flags_in;
    (void)flags_out;
    if (g_vsync) {
        g_vsync(context);
    }
    return kCVReturnSuccess;
}

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
int dcmax_vsync_start(DcmaxVsyncFn callback, void *context) {
    g_vsync = callback;
    if (g_link) {
        return 0;
    }
    CVReturn created = CVDisplayLinkCreateWithActiveCGDisplays(&g_link);
    if (created != kCVReturnSuccess || !g_link) {
        g_link = NULL;
        return -1;
    }
    CVDisplayLinkSetOutputCallback(g_link, dcmax_vsync_callback, context);
    CVReturn started = CVDisplayLinkStart(g_link);
    if (started != kCVReturnSuccess) {
        CVDisplayLinkRelease(g_link);
        g_link = NULL;
        return -1;
    }
    return 0;
}

void dcmax_vsync_stop(void) {
    if (!g_link) {
        return;
    }
    CVDisplayLinkStop(g_link);
    CVDisplayLinkRelease(g_link);
    g_link = NULL;
    g_vsync = NULL;
}
#pragma clang diagnostic pop
