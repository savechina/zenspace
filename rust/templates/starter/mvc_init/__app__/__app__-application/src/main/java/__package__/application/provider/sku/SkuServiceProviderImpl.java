package __package__.application.provider.sku;

import __package__.api.mo.ApiResult;
import __package__.application.convert.SkuDTOConvert;
import __package__.api.mo.SkuDTO;
import __package__.api.sku.SkuServiceProvider;
import __package__.domain.model.sku.SkuDO;
import __package__.domain.service.sku.SkuService;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Service;

@Service
public class SkuServiceProviderImpl implements SkuServiceProvider {

    /**
     * Sku Servicell
     */
    @Autowired
    private SkuService skuService;

    @Autowired
    private SkuDTOConvert skuDTOConvert;

    @Override
    public ApiResult<SkuDTO> getSkuById(Long skuId) {
        // 转换入参
        SkuDO param = skuDTOConvert.convert(new SkuDTO());

//        long skuId = 120L;
        SkuDO sku = skuService.getSku(skuId);

        SkuDTO dto = skuDTOConvert.reverseConvert(sku);
        return ApiResult.success(dto);
    }
}
